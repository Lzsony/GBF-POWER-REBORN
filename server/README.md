# Control 與 SSH Gateway

服務端使用 Go、SQLite 與 Bubble Tea。`reborn control` 和 `reborn gateway` 是獨立程序；同主機部署仍使用不同服務帳號、設定、金鑰與資料目錄。正式部署的每臺主機僅配置一個 Gateway，此限制由部署工具的主機識別、安裝登記與鎖定機制執行。

## 工具鏈與驗證

Go 版本及依賴以 `go.mod`、`go.sum` 為準。從本目錄執行：

```sh
go test ./...
go test -race ./...
go vet ./...
```

測試使用臨時 SQLite 資料庫、本機 HTTPS Control、SSH Gateway 與替代上游，不連接部署中的服務。兩個 Gateway fixture 用於驗證跨節點授權、撤銷與租約，不代表正式部署可在同一主機運行多個 Gateway。

建置版本由發布腳本透過 `-ldflags=-X main.version=VERSION` 注入；直接開發建置的版本為 `dev`。

## 程序接口

| 命令 | 行為 |
| --- | --- |
| `control --config PATH` | 啟動 HTTPS Control 與本機管理 socket |
| `gateway --config PATH` | 啟動 SSH 流量服務與租約／統計工作 |
| `admin --socket PATH` | 開啟 Bubble Tea 管理介面 |
| `admin --socket PATH --json` | 從 stdin 接收管理請求，向 stdout 輸出 JSON |
| `check-config --role control\|gateway --config PATH` | 檢查設定語法，不宣稱外部網路可用 |
| `doctor --role control\|gateway --config PATH` | 執行設定、身份、資料或連線診斷 |
| `init-control --dir PATH --host HOST` | 在服務端初始化 master key、TLS 金鑰與公開憑證 |
| `init-gateway --config PATH` | 在服務端初始化 Gateway identity key 與 SSH host key |
| `public-keys --config PATH` | 輸出節點識別與兩個公開金鑰 |
| `join --config PATH` | 從 stdin 讀取加入 ticket，向 Control 註冊 |
| `version` | 顯示建置版本 |

設定中的資料、金鑰、規則、統計及 socket 路徑須為絕對路徑。`admin` 預設 socket 為 `/run/gbf-control/admin.sock`。Gateway 的 SSH 協定帳號為 `reborn`，與執行服務的 OS 帳號不同。

## 管理與授權

TUI 以 `Tab` 切換帳號、裝置、節點及診斷頁；`n` 新增、`/` 搜尋、`F5` 更新。建立帳號時，管理者填寫名稱與裝置上限，再以 `Space` 勾選零個或多個已註冊且開放的節點。所有節點預設不勾選；建立帳號與初始授權位於同一 SQLite 交易，任一授權失敗即全部回復。

本機 JSON 管理接口對應的新增欄位為 `nodeIds`：

```json
{"action":"account-create","name":"Example user","maxDevices":2,"nodeIds":[]}
```

空清單建立沒有節點授權的帳號。`pending` 節點不列入初始選項，須另以 `grant-set` 的 `preview: true` 明確授予測試帳號。節點完成註冊並提供新鮮健康上報後才能設為 `enabled`。新增節點不會自動擴大既有帳號的授權。

授權碼只在明確操作後顯示，30 秒後隱藏；重設碼會更新帳號 epoch，使原裝置必須重新授權。裝置解除、帳號停用、節點停用及授權撤銷均影響後續租約。

## 協定與流量邊界

| 接口 | 用途 |
| --- | --- |
| `GET /health/live` | Control 存活檢查 |
| `POST /v1/activate` | 授權碼與裝置公鑰註冊 |
| `POST /v1/status` | 裝置狀態與可用節點 |
| `POST /v1/unbind` | 解除裝置 |
| `POST /internal/v1/join` | Gateway 註冊 |
| `POST /internal/v1/leases` | Gateway 取得裝置租約 |
| `POST /internal/v1/report` | Gateway 狀態及流量上報 |

協定版本為 1。請求使用 Ed25519 簽署路徑、時間戳、nonce、公鑰與 payload，並檢查重播與身份角色。Gateway 租約有效期為 45 秒，每 15 秒刷新；獨立過期工作不等待 Control 網路請求。Control 不承載遊戲流量。

Gateway 僅接受受授權的 SSH `direct-tcpip`，拒絕 shell、exec 及遠端埠轉發。目的地主機與網域後綴依部署規則判定，僅允許 80／443；DNS 解析結果再排除非公開位址。動態 TLS 流量保持透傳，不解析遊戲請求內容。

容量是管理者設定的參考帶寬，用於持續負載告警，不是限速承諾。上報過期時節點的健康、線上數與即時速率顯示未知；流量依 UTC 日期累計並持久保存。

## 驗證邊界

`doctor` 的本機監聽與憑證檢查不代表雲端防火牆或外部網路已可達。Gateway 的公開頁探測也不代表使用者裝置的完整授權、SSH、PAC 與遊戲流程已驗收；這些結果須由部署端及客戶端另行核對。
