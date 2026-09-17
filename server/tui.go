package main

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

type refreshMsg struct {
	snapshot Snapshot
	err      error
}
type responseMsg struct {
	value json.RawMessage
	err   error
}
type tickMsg time.Time
type clearSecretMsg struct{}
type field struct{ label, value string }
type nodeChoice struct {
	id, name string
	selected bool
}
type form struct {
	nodes   []nodeChoice
	action  string
	fields  []field
	focus   int
	base    AdminRequest
	confirm bool
}
type tui struct {
	socket                     string
	data                       Snapshot
	tab, cursor, width, height int
	filter                     string
	search                     bool
	form                       *form
	message, secret            string
	busy                       bool
}

func refresh(socket string) tea.Cmd {
	return func() tea.Msg {
		var s Snapshot
		err := adminCall(socket, AdminRequest{Action: "snapshot"}, &s)
		return refreshMsg{s, err}
	}
}
func call(socket string, q AdminRequest) tea.Cmd {
	return func() tea.Msg {
		var raw json.RawMessage
		err := adminCall(socket, q, &raw)
		return responseMsg{raw, err}
	}
}
func tick() tea.Cmd         { return tea.Tick(5*time.Second, func(t time.Time) tea.Msg { return tickMsg(t) }) }
func (m tui) Init() tea.Cmd { return tea.Batch(refresh(m.socket), tick()) }
func (m tui) items() []string {
	r := []string{}
	switch m.tab {
	case 0:
		for _, a := range m.data.Accounts {
			if strings.Contains(strings.ToLower(a.Name+" "+a.ID), strings.ToLower(m.filter)) {
				r = append(r, a.ID)
			}
		}
	case 1:
		for _, d := range m.data.Devices {
			if strings.Contains(strings.ToLower(d.Name+" "+d.ID+" "+d.AccountID), strings.ToLower(m.filter)) {
				r = append(r, d.ID)
			}
		}
	case 2:
		for _, n := range m.data.Nodes {
			if strings.Contains(strings.ToLower(n.Name+" "+n.ID), strings.ToLower(m.filter)) {
				r = append(r, n.ID)
			}
		}
	}
	return r
}
func (m tui) selection() string {
	ids := m.items()
	if m.cursor >= 0 && m.cursor < len(ids) {
		return ids[m.cursor]
	}
	return ""
}
func (m tui) account(id string) Account {
	for _, a := range m.data.Accounts {
		if a.ID == id {
			return a
		}
	}
	return Account{}
}
func (m tui) node(id string) Node {
	for _, n := range m.data.Nodes {
		if n.ID == id {
			return n
		}
	}
	return Node{}
}
func (m tui) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch v := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = v.Width
		m.height = v.Height
	case tickMsg:
		if m.busy {
			return m, tick()
		}
		return m, tea.Batch(refresh(m.socket), tick())
	case refreshMsg:
		if v.err != nil {
			m.message = v.err.Error()
			m.data.UnknownNodes = len(m.data.Nodes)
			m.data.ConfirmedOnline = 0
			for i := range m.data.Nodes {
				n := &m.data.Nodes[i]
				n.Fresh = false
				n.Healthy = nil
				n.Sessions = nil
				n.UploadBPS = nil
				n.DownloadBPS = nil
				n.Utilization = nil
				n.HighLoad = false
			}
		} else {
			m.data = v.snapshot
			if m.cursor >= len(m.items()) {
				m.cursor = max(0, len(m.items())-1)
			}
		}
	case responseMsg:
		m.busy = false
		if v.err != nil {
			m.message = v.err.Error()
			return m, nil
		}
		var values map[string]any
		_ = json.Unmarshal(v.value, &values)
		if code, ok := values["code"].(string); ok {
			m.secret = code
		}
		if token, ok := values["token"].(string); ok {
			m.secret = "節點加入憑證（10 分鐘）：" + token
		}
		m.message = "操作完成"
		if m.tab == 3 {
			m.message = string(v.value)
		}
		return m, tea.Batch(refresh(m.socket), tea.Tick(30*time.Second, func(time.Time) tea.Msg { return clearSecretMsg{} }))
	case clearSecretMsg:
		m.secret = ""
	case tea.KeyPressMsg:
		k := v.String()
		if k == "ctrl+c" {
			return m, tea.Quit
		}
		if k == "esc" {
			m.form = nil
			m.search = false
			m.filter = ""
			m.secret = ""
			return m, nil
		}
		if m.busy {
			return m, nil
		}
		if m.search {
			if k == "enter" {
				m.search = false
			} else if k == "backspace" {
				r := []rune(m.filter)
				if len(r) > 0 {
					m.filter = string(r[:len(r)-1])
				}
			} else if v.Text != "" {
				m.filter += v.Text
			}
			m.cursor = 0
			return m, nil
		}
		if m.form != nil {
			f := m.form
			total := len(f.fields) + len(f.nodes)
			if k == "tab" || k == "down" {
				f.focus = (f.focus + 1) % total
			} else if k == "shift+tab" || k == "up" {
				f.focus = (f.focus + total - 1) % total
			} else if f.focus >= len(f.fields) && (k == "space" || v.Text == " ") {
				choice := &f.nodes[f.focus-len(f.fields)]
				choice.selected = !choice.selected
			} else if k == "backspace" && f.focus < len(f.fields) {
				r := []rune(f.fields[f.focus].value)
				if len(r) > 0 {
					f.fields[f.focus].value = string(r[:len(r)-1])
				}
			} else if k == "ctrl+u" && f.focus < len(f.fields) {
				f.fields[f.focus].value = ""
			} else if k == "enter" {
				q, err := formRequest(*f)
				if err != nil {
					m.message = err.Error()
					return m, nil
				}
				m.form = nil
				m.busy = true
				return m, call(m.socket, q)
			} else if v.Text != "" && f.focus < len(f.fields) {
				f.fields[f.focus].value += v.Text
			}
			return m, nil
		}
		switch k {
		case "q":
			return m, tea.Quit
		case "tab":
			m.tab = (m.tab + 1) % 4
			m.cursor = 0
			m.filter = ""
			m.message = ""
			m.secret = ""
		case "up", "k":
			m.cursor = max(0, m.cursor-1)
		case "down", "j":
			m.cursor = min(max(0, len(m.items())-1), m.cursor+1)
		case "/":
			m.search = true
		case "f5":
			return m, refresh(m.socket)
		case "d":
			m.tab = 3
			m.busy = true
			return m, call(m.socket, AdminRequest{Action: "doctor"})
		default:
			id := m.selection()
			if m.tab == 0 {
				a := m.account(id)
				switch k {
				case "n":
					m.form = &form{action: "account-create", fields: []field{{"名稱", ""}, {"裝置上限", "2"}}}
					for _, node := range m.data.Nodes {
						if node.Status == "enabled" && node.Registered {
							m.form.nodes = append(m.form.nodes, nodeChoice{id: node.ID, name: node.Name})
						}
					}
				case "v":
					if id != "" {
						m.busy = true
						return m, call(m.socket, AdminRequest{Action: "account-code", ID: id})
					}
				case "r":
					if id != "" {
						m.form = &form{action: "account-reset", confirm: true, base: AdminRequest{ID: id}, fields: []field{{"重設後原連線須重新授權；輸入 YES", ""}}}
					}
				case "e":
					if id != "" {
						enabled := !a.Enabled
						m.form = &form{action: "account-set", confirm: true, base: AdminRequest{ID: id, Enabled: &enabled}, fields: []field{{fmt.Sprintf("設定啟用=%t；輸入 YES", enabled), ""}}}
					}
				case "l":
					if id != "" {
						m.form = &form{action: "account-limit", base: AdminRequest{ID: id}, fields: []field{{"裝置上限", strconv.Itoa(a.MaxDevices)}}}
					}
				case "g":
					if id != "" {
						m.form = &form{action: "grant-set", base: AdminRequest{ID: id}, fields: []field{{"節點 ID", ""}, {"待驗收測試帳號 true/false", "false"}, {"移除授權 true/false", "false"}}}
					}
				}
			} else if m.tab == 1 && k == "x" && id != "" {
				m.form = &form{action: "device-revoke", confirm: true, base: AdminRequest{ID: id}, fields: []field{{"解除此裝置；輸入 YES", ""}}}
			} else if m.tab == 2 {
				n := m.node(id)
				switch k {
				case "n":
					m.form = &form{action: "node-add", fields: []field{{"唯一節點 ID", ""}, {"節點名稱", ""}, {"公開 IP／主機", ""}, {"SSH 埠", "2222"}, {"參考帶寬 Mbps（0=未設定）", "0"}}}
				case "t":
					if id != "" {
						m.busy = true
						return m, call(m.socket, AdminRequest{Action: "node-ticket", ID: id})
					}
				case "e":
					if id != "" {
						status := "enabled"
						if n.Status == "enabled" {
							status = "disabled"
						} else if n.Status == "disabled" {
							status = "pending"
						}
						m.form = &form{action: "node-set", confirm: true, base: AdminRequest{ID: id, Status: status, CapacityBPS: n.CapacityBPS, WarnPercent: n.WarnPercent, WarnSeconds: n.WarnSeconds}, fields: []field{{"設定節點為 " + status + "；輸入 YES", ""}}}
					}
				case "l":
					if id != "" {
						m.form = &form{action: "node-capacity", base: AdminRequest{ID: id, Status: n.Status}, fields: []field{{"參考帶寬 Mbps（0=未設定）", strconv.FormatInt(n.CapacityBPS/1000000, 10)}, {"預警百分比", strconv.FormatFloat(n.WarnPercent, 'f', -1, 64)}, {"持續秒數", strconv.Itoa(n.WarnSeconds)}}}
					}
				}
			}
		}
	}
	return m, nil
}
func formRequest(f form) (AdminRequest, error) {
	q := f.base
	q.Action = f.action
	if f.confirm {
		if f.fields[0].value != "YES" {
			return q, fmt.Errorf("請輸入 YES 或 Esc 取消")
		}
		return q, nil
	}
	integer := func(i int) (int, error) { return strconv.Atoi(f.fields[i].value) }
	var err error
	switch f.action {
	case "account-create":
		q.NodeIDs = make([]string, 0, len(f.nodes))
		for _, node := range f.nodes {
			if node.selected {
				q.NodeIDs = append(q.NodeIDs, node.id)
			}
		}
		q.Name = f.fields[0].value
		q.MaxDevices, err = integer(1)
	case "account-limit":
		q.Action = "account-set"
		q.MaxDevices, err = integer(0)
	case "grant-set":
		q.NodeID = f.fields[0].value
		q.Preview, err = strconv.ParseBool(f.fields[1].value)
		if err == nil {
			q.Remove, err = strconv.ParseBool(f.fields[2].value)
		}
	case "node-add":
		q.ID = f.fields[0].value
		q.Name = f.fields[1].value
		q.Host = f.fields[2].value
		q.Port, err = integer(3)
		if err == nil {
			var v int64
			v, err = strconv.ParseInt(f.fields[4].value, 10, 64)
			if v < 0 || v > 1000000 {
				return q, fmt.Errorf("invalid bandwidth")
			}
			q.CapacityBPS = v * 1000000
		}
	case "node-capacity":
		q.Action = "node-set"
		var v int64
		v, err = strconv.ParseInt(f.fields[0].value, 10, 64)
		if v < 0 || v > 1000000 {
			return q, fmt.Errorf("invalid bandwidth")
		}
		q.CapacityBPS = v * 1000000
		if err == nil {
			q.WarnPercent, err = strconv.ParseFloat(f.fields[1].value, 64)
		}
		if err == nil {
			q.WarnSeconds, err = integer(2)
		}
	}
	if err != nil {
		return q, fmt.Errorf("欄位格式無效")
	}
	return q, nil
}
func (m tui) View() tea.View {
	var b strings.Builder
	fmt.Fprintln(&b, "Reborn 管理端   Tab 切換｜↑↓ 選擇｜/ 搜尋｜F5 更新｜d 診斷｜q 離開")
	tabs := []string{"帳號", "裝置", "節點", "診斷"}
	for i, t := range tabs {
		if i == m.tab {
			fmt.Fprintf(&b, "[%s]  ", t)
		} else {
			fmt.Fprintf(&b, " %s   ", t)
		}
	}
	fmt.Fprintf(&b, "\n已確認在線 %d 台；%d 個節點資料未知\n", m.data.ConfirmedOnline, m.data.UnknownNodes)
	if m.search || m.filter != "" {
		fmt.Fprintln(&b, "搜尋："+m.filter)
	}
	if m.form != nil {
		return m.formView(b.String())
	}
	ids := m.items()
	rows := max(4, m.height-11)
	if m.tab == 2 {
		rows = max(2, m.height-17)
	}
	start := max(0, m.cursor-rows+1)
	end := min(len(ids), start+rows)
	for i := start; i < end; i++ {
		id := ids[i]
		prefix := "  "
		if i == m.cursor {
			prefix = "> "
		}
		var row string
		switch m.tab {
		case 0:
			a := m.account(id)
			state := "停用"
			if a.Enabled {
				state = "啟用"
			}
			row = fmt.Sprintf("%s  %s  %d/%d 台  %s", a.Name, state, a.Devices, a.MaxDevices, a.ID[:8])
		case 1:
			for _, d := range m.data.Devices {
				if d.ID == id {
					state := "有效"
					if d.Revoked {
						state = "已解除"
					} else if d.NeedsAuthorization {
						state = "需重新授權"
					}
					row = fmt.Sprintf("%s  %s  %s  %s", d.Name, m.account(d.AccountID).Name, state, d.Fingerprint)
				}
			}
		case 2:
			n := m.node(id)
			state := "未知／過期"
			count := "—"
			rate := "—"
			if n.Fresh {
				state = "健康"
				if n.Healthy != nil && !*n.Healthy {
					state = "異常"
				}
				if n.Sessions != nil {
					count = strconv.Itoa(len(*n.Sessions))
				}
				if n.UploadBPS != nil {
					rate = fmt.Sprintf("↑%.2f ↓%.2f Mbps", *n.UploadBPS/1e6, *n.DownloadBPS/1e6)
				}
			}
			capacity := "容量未設定"
			if n.Utilization != nil {
				capacity = fmt.Sprintf("%.1f%%", *n.Utilization)
			}
			if n.HighLoad {
				capacity += " 高負載"
			}
			row = fmt.Sprintf("%s %s [%s] %s %s 台 %s %s", n.ID, n.Name, n.Status, state, count, rate, capacity)

		}
		fmt.Fprintln(&b, prefix+ansi.Truncate(row, max(20, m.width-3), "…"))
	}
	switch m.tab {
	case 0:
		fmt.Fprintln(&b, "n 新增｜v 顯示碼｜r 重設碼｜e 啟停｜l 裝置額度｜g 線路授權")
	case 1:
		fmt.Fprintln(&b, "x 解除裝置（不永久封禁硬體）")
	case 2:
		fmt.Fprintln(&b, "n 新節點｜t 重發加入憑證｜e 待驗收/開放/停用｜l 容量；建議觀察 24 小時")
		if id := m.selection(); id != "" {
			n := m.node(id)
			last := "尚未上報"
			if n.LastReport > 0 {
				last = time.Unix(n.LastReport, 0).UTC().Format(time.RFC3339)
			}
			fmt.Fprintf(&b, "最後上報 %s；新鮮=%t\n", last, n.Fresh)
			fmt.Fprintf(&b, "配置容量 %.2f Mbps；預警 %.1f%% 持續 %d 秒\n", float64(n.CapacityBPS)/1e6, n.WarnPercent, n.WarnSeconds)
			for i, d := range n.Days {
				if i >= 3 {
					break
				}
				fmt.Fprintf(&b, "%s UTC 上行 %.2f MiB／下行 %.2f MiB\n", d.Day, float64(d.Up)/(1<<20), float64(d.Down)/(1<<20))
			}
		}
	case 3:
		fmt.Fprintln(&b, "d 執行唯讀診斷；外部可達性須另外由管理機驗證")
	}
	if m.secret != "" {
		fmt.Fprintln(&b, "\n"+m.secret+"\nEsc 隱藏（30 秒後自動隱藏）")
	}
	if m.busy {
		fmt.Fprintln(&b, "處理中…")
	} else if m.message != "" {
		if m.tab == 3 {
			var checks []Diagnostic
			if json.Unmarshal([]byte(m.message), &checks) == nil {
				for _, d := range checks {
					fmt.Fprintln(&b, ansi.Truncate(fmt.Sprintf("%s [%s] %s", d.Check, d.Status, d.Detail), max(20, m.width), "…"))
				}
			} else {
				fmt.Fprintln(&b, m.message)
			}
		} else {
			fmt.Fprintln(&b, m.message)
		}
	}
	v := tea.NewView(b.String())
	v.AltScreen = true
	return v
}

func (m tui) formView(header string) tea.View {
	var b strings.Builder
	b.WriteString(header)
	fmt.Fprintln(&b, "\nTab／↑↓ 切欄位｜Space 選節點｜Enter 建立／送出｜Esc 取消")
	f := m.form
	rows := make([]string, 0, len(f.fields)+len(f.nodes))
	for _, field := range f.fields {
		rows = append(rows, field.label+"："+field.value)
	}
	selected := 0
	for _, node := range f.nodes {
		mark := "[ ]"
		if node.selected {
			mark = "[x]"
			selected++
		}
		rows = append(rows, mark+" "+node.name+" ("+node.id+")")
	}
	if f.action == "account-create" {
		fmt.Fprintf(&b, "初始節點授權：已選 %d／%d，可不選；未選節點不會自動授權。\n", selected, len(f.nodes))
	}
	visible := max(1, m.height-8)
	start := max(0, f.focus-visible+1)
	end := min(len(rows), start+visible)
	for i := start; i < end; i++ {
		prefix := "  "
		if i == f.focus {
			prefix = "> "
		}
		fmt.Fprintln(&b, ansi.Truncate(prefix+rows[i], max(20, m.width-1), "…"))
	}
	if start > 0 || end < len(rows) {
		fmt.Fprintf(&b, "顯示 %d–%d／%d\n", start+1, end, len(rows))
	}
	if m.message != "" {
		fmt.Fprintln(&b, ansi.Truncate(m.message, max(20, m.width-1), "…"))
	}
	view := tea.NewView(b.String())
	view.AltScreen = true
	return view
}
