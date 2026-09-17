# 部署指引

這份指引從一臺 Linux 主機開始，部署 Control 與 Gateway，最後建立可供客戶端使用的授權。Control 管理帳號，Gateway 轉送流量；兩者在同機仍為獨立服務。

文中的「本機」是保存專案原始碼、執行部署工具的電腦；「伺服器」是你租用的 Linux 主機。命令未另註明時，均在**本機專案根目錄**執行。

## 1. 準備主機與管理連線

準備一臺 Debian 12／13 或 Ubuntu 24.04／26.04 主機，使用 amd64 或 arm64，並運行 systemd。你需要公網 IP 或可解析的主機名稱，以及可使用 `sudo -n` 的管理 SSH 帳號。

**本機**需要 Python 3.9+、OpenSSH 的 `ssh`／`ssh-keyscan`；建置服務端還需要 Go，見[建置指引](build-guide.md#linux-服務端)。部署工具只檢查本機需要的工具，不安裝本機工具鏈。Windows 管理機使用已安裝的 `python` 命令代替本文的 `python3`，或在 WSL 中執行；不要混用兩邊的 SSH 設定與檔案路徑。

在本機 SSH 設定加入別名，替換 `HostName` 與 `User`：

```sshconfig
Host gpr-node
    HostName node.example.com
    User admin
```

先透過主機供應商的可信管道核對 SSH 指紋，再首次登入。不要關閉 host-key 檢查。確認可以執行：

```sh
ssh gpr-node 'sudo -n true'
```

**伺服器**先準備可用的管理 SSH、`sudo -n`、APT、基本系統工具及正常運行的 systemd。遠端 Python、OpenSSL、OpenSSH 用戶端、iproute2、帳號管理工具、tar 或系統 CA 缺少時，部署工具會在套用階段自動補齊；初始檢查不依賴 Python。時間同步、主機名稱解析與防火牆仍由部署者準備。

**成功判據：** SSH 可直接登入，`sudo -n true` 成功退出，無需輸入密碼。

## 2. 建置服務端並填寫配置

**本機**依[Linux 服務端建置](build-guide.md#linux-服務端)生成 release，再複製範例：

```sh
cp deploy/topology.example.json deploy/topology.local.json
```

將本機副本改成以下內容。`node.example.com` 須換成伺服器真實網域或 IPv4 位址；同機的兩個 `sshTarget` 都使用 `gpr-node`。

```json
{
  "schemaVersion": 1,
  "deploymentId": "my-gpr",
  "releaseRoot": "../artifacts/server",
  "outputDir": "../artifacts/deployment",
  "control": {
    "sshTarget": "gpr-node",
    "publicUrl": "https://node.example.com",
    "port": 443,
    "managed": true
  },
  "gateways": [
    {
      "sshTarget": "gpr-node",
      "id": "node-1",
      "name": "我的節點",
      "publicHost": "node.example.com",
      "port": 2222
    }
  ]
}
```

`deploymentId` 是這次部署的穩定名稱，後續更新沿用；不要填授權碼。若建置時另設輸出目錄，修改 `releaseRoot`。相對路徑以配置檔所在目錄為準；每種所需架構只能放一個 release 版本。

## 3. 預檢、開放埠、部署

**本機**執行唯讀預檢：

```sh
python3 deploy/manage.py --config deploy/topology.local.json --plan
```

確認輸出包含 `control`、`gateway`、正確架構及入站 TCP `443`／`2222`。`dependencies` 會列出缺失能力與對應套件；`preflight: incomplete` 表示仍有檢查未完成，例如缺少 `ss` 時尚未核對埠占用，不能視為埠已空閒。

`--plan` 不安裝或更新套件。`--apply` 會先核對本機 release，再自動補齊依賴並完成預檢。若想先處理依賴，在本機執行以下命令後重跑 `--plan`：

```sh
python3 deploy/manage.py --config deploy/topology.local.json --install-deps
```

`--install-deps` 不要求 release、不部署或重啟服務。已齊全時不執行 APT；缺失時才更新索引並安裝必要套件。若套件已安裝但命令遺失，或安裝需要升級／移除其他套件，工具會停止並提示人工修復。APT 日誌保存在伺服器的 root 專用目錄，錯誤訊息會指出位置。

在主機防火牆及供應商的雲端防火牆開放這兩個服務埠，保留原管理 SSH 埠。工具不會代替你修改防火牆。

若埠已被舊服務占用，先決定舊服務的保留或替換方式，不要直接覆蓋。首次部署的主線以空閒服務埠為前提。

```sh
python3 deploy/manage.py --config deploy/topology.local.json --apply
python3 deploy/manage.py --config deploy/topology.local.json --verify
```

**成功判據：** `--apply` 回傳 `runId` 與 `clientProfile`；`--verify` 顯示 `controlTls` 和 `gatewayHostKeys` 為 `passed`。新節點此時仍是 `pending`，尚不能供一般帳號使用。

部署亦會安裝 `gpr` 管理命令。公開客戶端配置與公開憑證在 `artifacts/deployment/`；它們是後續建置客戶端所需的兩個檔案，不是帳號授權。

## 4. 開放節點、建立帳號

**本機**直接開啟遠端管理介面：

```sh
ssh -t gpr-node gpr admin
```

1. 按 `Tab` 切到節點頁，確認新節點已註冊、上報新鮮且健康，依頁面提示設為開放。
2. 回到帳號頁，按 `n` 新增帳號，填寫名稱與裝置上限。
3. 用 `Space` 勾選可用節點，再提交；未勾選節點的帳號沒有加速使用權。
4. 選取帳號按 `v` 查看授權碼，供客戶端啟用。代碼短暫顯示，不要放進部署配置或原始碼。

按 `q` 退出管理介面，伺服器繼續運行。接著閱讀[建置指引](build-guide.md#自建版客戶端的公開配置)，製作連向這次部署的客戶端。

## 日常管理

登入**伺服器**後使用下列命令；也可從本機加上 `ssh gpr-node` 前綴。只有互動 TUI 要加 `ssh -t`。

| 命令 | 用途 |
| --- | --- |
| `gpr admin` | 帳號、裝置、節點管理 |
| `gpr status` | 查看所有已安裝服務 |
| `gpr logs gateway` | 最近 100 行 Gateway 日誌 |
| `gpr logs gateway -f` | 持續追蹤；Ctrl+C 結束，不停止服務 |
| `gpr doctor` | 列出通過、失敗、未檢查的診斷項目 |
| `gpr restart gateway` | 重啟 Gateway，會中斷該節點目前連線 |
| `gpr stop all`／`gpr start all` | 停止／啟動已安裝服務 |
| `gpr --help` | 查看完整用法 |

`status`、`logs`、`doctor` 可指定 `control`、`gateway` 或 `all`，省略時為所有已安裝角色；`start`、`stop`、`restart` 必須指定對象。`all` 啟動時先 Control 後 Gateway，停止時相反。`restart all` 先按停止順序停下，再按啟動順序啟動。權限沿用管理帳號的 `sudo -n`，不額外修改 sudoers。`doctor` 有失敗項目時退出碼非零；未檢查項目仍需另行驗收。

### 舊部署補裝管理命令

若新版服務已部署，但找不到 `gpr`，在**本機**執行：

```sh
python3 deploy/manage.py --config deploy/topology.local.json --install-manager
```

這個模式會先補齊缺失依賴，再安裝／更新管理入口；不需要 release、不呼叫服務啟停，也不更改帳號、金鑰或資料。若同名路徑已被其他程式或符號連結占用，工具會拒絕覆寫；確認用途並處理衝突後重試。

## 更新、增加節點與問題處理

- **更新：** 先按建置指引生成新版本，讓 `releaseRoot` 指向只有本次版本的目錄，再執行 `--plan`、`--apply`、`--verify`。更新會重新啟動相關服務，安排在可中斷時操作。
- **增加節點：** 每臺主機最多一個 Gateway。新增主機及拓撲項目後先 `--plan`，再 `--apply --add-node node-2`；完成後開放節點，明確分配給需要的帳號。
- **僅管理入口失敗：** 若提示 `manager-install-failed`，服務與公開 profile 已完成部署，修復衝突後執行 `--install-manager`，不必重跑服務安裝。原 journal 保留當次失敗狀態，補裝結果另行核對。
- **註冊或外部驗證失敗：** 先檢查公開地址、防火牆與 `gpr doctor`，修復後重跑 `--apply`；工具保留本次身份供重試。
- **撤回：** 使用 `--rollback RUN_ID`，保留原配置的 `outputDir` 以找到 journal。撤回不回退業務資料庫、不取消節點註冊，也不移除共用 `gpr` 命令；`gpr` 會使用撤回後的 `current` 執行檔。
- **權限錯誤：** 檢查管理 SSH 帳號是否仍可執行 `sudo -n true`。Gateway-only 主機不能開啟 `gpr admin`，須連到 Control 主機。
- **套件安裝失敗：** 排除鎖占用、來源或 dpkg 狀態問題後重跑 `--install-deps`。每臺主機安裝階段最長 15 分鐘，套件鎖最多等待 120 秒；不會強制移除鎖檔或自動執行整機升級。已安裝的套件不會由部署 rollback 卸載。
- **系統 CA 缺失且 APT 僅使用 HTTPS：** 若因此無法更新套件索引，須先透過可信管道修復系統 CA；工具不會關閉套件來源的 TLS 驗證。

多主機與撤回的完整邊界見[部署規格](self-hosted-gateway.md)；瀏覽器 PAC、授權、憑證與快取操作見[客戶端操作](client.md)。
