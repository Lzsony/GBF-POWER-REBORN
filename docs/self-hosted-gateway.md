# 自建服務部署與客戶端建置

0.4.0 提供 Linux Control、SSH Gateway、管理 TUI 及配置驅動的部署工具。部署者自備主機與管理 SSH，完成服務部署後，將公開服務 profile 內嵌到自行建置的客戶端，再分發給使用者。專案不提供公共節點。

首次操作見[部署指引](deployment-guide.md)與[建置指引](build-guide.md)；本文保留拓撲、隔離、配置及失敗處理的技術契約。

## 拓撲與服務隔離

| 拓撲 | 配置 |
| --- | --- |
| 單節點 | 一臺主機運行獨立 Control 與一個 Gateway |
| 多節點 | 一個 Control 管理多臺 Gateway 主機；Control 所在主機也可運行一個 Gateway |
| Control 獨立主機 | Control 與所有 Gateway 分機，Control 不承載遊戲流量 |

每臺主機最多一個 Gateway。不同管理 SSH 別名不視為不同主機：預檢讀取遠端 `/etc/machine-id` 的用途限定雜湊，按此合併主機並檢查 Gateway ID、架構及服務埠。此處主機指獨立 OS／VM 執行環境；識別不能證明不同 VM 是否共用底層硬體。套用時取得主機部署鎖後再次驗證。

| 項目 | Control | Gateway |
| --- | --- | --- |
| systemd unit／OS 帳號 | `gbf-control.service`／`gbf-control` | `gbf-gateway.service`／`gbf-gateway` |
| 設定與金鑰 | `/etc/gbf-reborn/control/` | `/etc/gbf-reborn/gateway/` |
| 執行資料 | `/var/lib/gbf-reborn/control/` | `/var/lib/gbf-reborn/gateway/` |
| 版本與目前執行檔 | `/opt/gbf-reborn/control/releases/`、`current` | `/opt/gbf-reborn/gateway/releases/`、`current` |

Gateway 的 SSH 協定帳號為 `reborn`，與 OS 服務帳號不同。兩個服務透過 HTTPS 交換必要資訊，不共用資料庫、私鑰或版本指向。

## 環境與配置

服務主機支援 Debian 12／13、Ubuntu 24.04／26.04，架構為 amd64／arm64，須運行 systemd。預檢要求 Python 3、`systemctl`、`openssl`、`ssh-keygen`、`ss`、`useradd`、`runuser`、`tar` 及可用的非互動 `sudo -n`。

管理機需要 Python 3、OpenSSH `ssh`／`ssh-keyscan`，並先完成管理 SSH 的 host-key 驗證與登入配置。主機上的套件、時間同步、防火牆與雲端安全組由部署者準備；工具不安裝套件、不修改 sshd、防火牆或 sysctl。

先建置 Linux 產物，再建立部署配置：

```sh
python3 scripts/build-server.py --arch all
cp deploy/topology.example.json deploy/topology.local.json
```

Go 版本以 `server/go.mod` 為準；工具鏈不在 PATH 時使用 `--go /absolute/path/to/go`。`--arch` 接受 `amd64`、`arm64`、`all`，`--output-dir` 可指定輸出位置。預設產物位於 `artifacts/server/gbf-server-0.4.0-linux-<arch>/`，包含二進位、共用目的地規則、授權與 hash manifest；同名已存在時不覆寫。

編輯 [topology.example.json](../deploy/topology.example.json) 的本機副本：

- `deploymentId`：由部署者指定的穩定識別。更換同一部署的 Control URL 或公開憑證時保持此值，客戶端會清除舊授權與節點快照並要求重新啟用；不同部署使用不同 ID。
- `control`：管理 SSH 目標、Control HTTPS origin、監聽埠，以及是否由本次操作安裝／更新。`publicUrl` 的埠須與 `port` 相符。
- `gateways`：每個節點的管理 SSH、唯一 `id`、顯示名稱、公開主機及 SSH 服務埠。
- `releaseRoot`：每個所需架構恰好一個版本的服務端產物目錄；全部架構版本須一致。
- `outputDir`：本機 journal 與公開客戶端建置輸出的保存位置。相對路徑以配置檔所在目錄解析。

單節點將 Control 與 Gateway 的 `sshTarget` 指向同一主機，並使用不同服務埠；多節點在 `gateways` 增加不同主機。既有 Control 可設 `managed: false` 跳過安裝，但仍須具有可存取的管理 SSH、既定 Control 服務及本機 admin socket。節點 ID 由部署者命名，不依地域自動建立或授權。

## 預檢、部署與驗證

```sh
python3 deploy/manage.py --config deploy/topology.local.json --plan
python3 deploy/manage.py --config deploy/topology.local.json --apply
python3 deploy/manage.py --config deploy/topology.local.json --verify
```

`--plan` 連接管理 SSH 進行唯讀預檢，檢查主機身份、OS／架構、工具、已安裝角色、埠占用及 release，輸出各主機角色與必需的入站 TCP 埠。依計畫開放相應的主機及雲端網路規則，再執行 `--apply`。

`--apply` 驗證傳輸後的產物，按角色保存快照並切換版本，於服務主機生成 Control／Gateway 私鑰，使用短期加入 ticket 註冊節點。ticket 僅在程序記憶體及 SSH stdin 傳遞，不寫入本機配置。成功後驗證公開 Control TLS 存活及 Gateway 公開 SSH host key，產生：

- `outputDir/client-build.local.json`：部署識別、Control URL 與公開憑證相對檔名。
- `outputDir/control-public.crt`：Control 公開憑證。
- `outputDir/run-<runId>.local.json`：操作 journal 與各主機快照識別。

`--verify` 檢查服務狀態與 doctor 結果，並從管理機核對 Control 公開 TLS 入口與 Gateway host key。這不代替使用者裝置的授權、PAC 或完整遊戲流程驗收。

### 管理命令安裝

`--apply` 完成服務部署、註冊、外部驗證及公開 profile 匯出後，於本次相關主機安裝 `/usr/local/bin/gpr`。入口獨立於 release，呼叫對應角色的 `current` 執行檔。既有部署可執行：

```sh
python3 deploy/manage.py --config deploy/topology.local.json --install-manager
```

此模式依拓撲與主機身份去重，取得部署鎖後再次核對身份；不需要 release、不切換服務版本、不重啟服務。程式以固定路徑及參數呼叫系統工具，沿用管理帳號的 `sudo -n`，不修改 sudoers。入口檔及其父目錄須由 root 管理、不可由群組或其他使用者寫入；安裝拒絕同名非受管理檔案及符號連結，通過 hash 核對後原子替換。

`gpr` 提供 `admin`、`status`、`logs`、`doctor`、`start`、`stop`、`restart`。查詢預設所有已安裝角色；啟停須明確指定角色或 `all`。`logs` 預設最近 100 行，`-f` 持續追蹤。`doctor` 區分通過、失敗及未檢查；存在失敗時回傳非零退出碼。完整用法見[日常管理](deployment-guide.md#日常管理)。

管理入口安裝失敗時，journal 記為 `manager-install-failed`，已驗證服務與匯出的 profile 保留。排除同名路徑或權限問題後單獨重試 `--install-manager`；原 journal 保留歷史失敗狀態，該重試不重啟或撤回服務。角色撤回不移除共用入口，既有入口繼續使用撤回後的角色執行檔。

## 節點與帳號管理

新節點註冊後保持 `pending`。在 Control 主機執行管理 TUI：

```sh
gpr admin
```

在節點頁確認身份、健康上報與診斷，再明確開放節點。若需要先驗收 pending 節點，可另對測試帳號授予 preview。新增帳號時，TUI 讓管理者勾選零個以上已開放節點；帳號與初始 grants 在同一交易建立，不自動授予任何節點。

追加 Gateway 時，先將新主機加入同一份拓撲，執行預檢後指定節點：

```sh
python3 deploy/manage.py --config deploy/topology.local.json --plan
python3 deploy/manage.py --config deploy/topology.local.json --apply --add-node gateway-2
```

`gateway-2` 須與配置中的 ID 一致。相同身份重跑保留金鑰；不同 ID 不得覆寫同主機既有 Gateway。加入 ticket 失效或網路失敗時保留身份與 pending 狀態，修復後重跑。新增節點不自動擴大既有帳號的 grants。

## 內嵌公開服務配置建置

部署成功時自動匯出公開 profile；已有服務可獨立匯出：

```sh
python3 deploy/manage.py --config deploy/topology.local.json --export-client
```

此操作透過已驗證的管理 SSH 讀取 Control 公開憑證，不要求 Gateway 可達。也可依 [client-build.example.json](../deploy/client-build.example.json) 建立 `client-build.local.json`；只允許 `deploymentId`、`url`、`caCertificateFile` 三個欄位。憑證須位於配置目錄內，內容只能是一張公開 PEM 憑證。部署工具沿用 topology 中的 `deploymentId`。

macOS arm64：

```sh
export GPR_CLIENT_CONFIG="$(pwd)/artifacts/deployment/client-build.local.json"
npm run package:macos
```

Windows x64（PowerShell）：

```powershell
$env:GPR_CLIENT_CONFIG = (Resolve-Path artifacts/deployment/client-build.local.json).Path
npm ci
npm run tauri -- build --no-bundle
python scripts/package-windows.py
```

若調整 `outputDir`，建置變數亦須指向對應檔案。建置器將經驗證的 profile 編入執行檔，封裝器核對內嵌標記與本次輸入；不複製完整部署配置或秘密。使用者只需在已配置服務的客戶端輸入授權碼，不新增服務地址設定或配置匯入入口。

未設定 `GPR_CLIENT_CONFIG` 時產生通用版，加速停用，直連與上游代理正常使用。建置通用版前應移除該環境變數。公開 profile 不是帳號授權；分發客戶端不會使使用者自動取得節點存取權。

## 更新、撤回與驗收邊界

更新時使用新版本 release，預檢後重跑 `--apply`。Control 與 Gateway 的版本指向分開更新，配置及身份保留。需要撤回時，使用 journal 的 `runId`：

```sh
python3 deploy/manage.py --config deploy/topology.local.json --rollback RUN_ID
```

撤回還原程式指向、配置及服務啟停狀態，不回退 SQLite 業務資料，也不撤銷已完成的 Control 節點註冊；身份材料保留供重跑。未提供完整卸載或跨版本資料庫降版工具。

公開交付驗收須分開記錄本機測試、Linux 原生 systemd、跨主機控制、外部 TLS／SSH、客戶端授權與實際流量。已完成項目見[驗證紀錄](validation.md)，資料位置與保存範圍見[隱私規格](privacy.md)。
