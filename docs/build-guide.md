# 建置指引

這份指引分成服務端與桌面客戶端。所有命令在**本機專案根目錄**執行，無需登入伺服器編譯。

| 要製作的產物 | 建置環境 | 輸出 |
| --- | --- | --- |
| Linux Control／Gateway | 安裝 Go 的管理機，可交叉建置 | `artifacts/server/` |
| macOS 客戶端 | Apple Silicon Mac、Xcode | `artifacts/macos/` 的 App／DMG |
| Windows 客戶端 | Windows x64、MSVC Build Tools、Windows SDK | `artifacts/windows/` 的 ZIP |

服務端與客戶端工具鏈分開準備；只建置服務端不需要 Node.js 或 Rust。

## Linux 服務端

**本機準備：** Python 3、Go 1.27.0 或符合 `server/go.mod` 的工具鏈。先確認：

```sh
python3 --version
go version
```

Windows 本機使用 `python` 代替 `python3`。依伺服器 CPU 選擇建置目標：

```sh
# 一般 x64 伺服器
python3 scripts/build-server.py --arch amd64

# ARM64 伺服器；只在需要此架構時執行
python3 scripts/build-server.py --arch arm64
```

也可用一次 `--arch all` 同時建置兩種架構。若 Go 不在 PATH，使用 `--go` 並填入**絕對路徑**，例如 `--go /absolute/path/to/go`。

**成功判據：** 終端顯示 `Verified release`。每個 `gbf-server-<版本>-linux-<架構>/` 目錄包含執行檔、目的地規則、授權文件及 hash manifest；版本由專案套件版本產生，不需手動改檔名。

建置器不覆寫已存在的同名產物。重建時換用新輸出目錄：

```sh
python3 scripts/build-server.py --arch amd64 --output-dir artifacts/server-next
```

部署配置中的 `releaseRoot` 必須一起改為 `../artifacts/server-next`。不要把多個版本放在同一 releaseRoot；部署工具要求每個所需架構只有一個版本。建置成功後前往[部署指引](deployment-guide.md#2-建置服務端並填寫配置)。

## 自建版客戶端的公開配置

先完成伺服器部署。成功的 `--apply` 會在拓撲的 `outputDir` 產生：

```text
client-build.local.json
control-public.crt
```

預設位置是 `artifacts/deployment/`。已有服務也可在**本機**重新匯出：

```sh
python3 deploy/manage.py --config deploy/topology.local.json --export-client
```

將這兩個檔案一起交給建置電腦，保持相對位置。公開憑證只提供服務信任，授權碼與私鑰不放入建置材料；無需將整份 topology 或 journal 交給客戶端使用者。

自建版靠 `GPR_CLIENT_CONFIG` 指定上述 JSON，在建置時內嵌服務地址及公開憑證。使用者拿到 App 後只需輸入授權碼。沒有這個環境變數會建成**通用版**，加速停用；不是安裝後再填服務地址。

## macOS arm64 客戶端

**建置電腦：** Apple Silicon Mac，準備 Node.js 22+、npm、Rust／Cargo、Python 3 與 Xcode，並完成 Xcode 命令列工具設定。先執行前置檢查：

```sh
python3 scripts/package-macos.py --check-only
```

看到 `PASS` 後，指定公開 profile 再打包：

```sh
export GPR_CLIENT_CONFIG="$(pwd)/artifacts/deployment/client-build.local.json"
npm run package:macos
```

若 `outputDir` 不同，替換 JSON 的路徑。打包入口會安裝鎖定依賴、建置程式、收集授權文件，並檢查版本、arm64、公開 profile、簽章及 DMG 內容；不用先另外執行 `npm ci`。

**成功判據：** 顯示 `PASS: verified App/DMG`。交付物位於 `artifacts/macos/`：

- `GBF POWER REBORN.app`
- `GBF-POWER-REBORN-<版本>-macos-arm64.dmg` 與 `.sha256`
- `manifest.json`

開啟 DMG，將 App 放入 Applications 後啟動。打包工具不會自動安裝或重設本機資料。產物使用 ad-hoc 簽章，未經 Apple 公證；如系統阻擋，先核對產物來源及 SHA-256，再透過 macOS 安全設定處理。本指引不涵蓋 Intel Mac 建置。

## Windows x64 客戶端

**建置電腦：** Windows x64，準備 Node.js 22+、npm、Rust 的 MSVC 工具鏈、Python 3、MSVC Build Tools 的 C++ 桌面開發元件及 Windows SDK。執行電腦需要 Microsoft Edge WebView2 Runtime。

在 **PowerShell** 執行；確認 `python` 可以啟動 Python 3：

```powershell
$env:GPR_CLIENT_CONFIG = (Resolve-Path artifacts/deployment/client-build.local.json).Path
npm ci
npm run tauri -- build --no-bundle
python scripts/package-windows.py
```

**成功判據：** 建置與封裝命令成功退出，`artifacts/windows/` 出現版本對應的 ZIP 及 SHA-256。ZIP 包含 EXE、manifest 與授權文件；解壓縮後執行 `GBF POWER REBORN.exe`。

Windows 產物未做 Authenticode 簽署。這是 Windows 原生建置步驟，不代表已完成本版本 Windows 真機驗收；目前實測範圍以[驗證紀錄](validation.md)為準。

## 安裝後的五步檢查

1. 確認自建版有「授權」入口，在代理停止時輸入部署者提供的授權碼。
2. 選擇加速模式與可用節點，按「啟動」，確認已連線。
3. 在瀏覽器代理插件的 PAC 情境填入 `http://127.0.0.1:8123/proxy.pac`，更新並套用。
4. 如需 HTTPS 靜態快取，先停止代理，從「管理憑證」安裝並信任本機 CA，開啟快取後重新啟動。
5. 實際登入遊戲並操作，核對連線、下載與快取命中。停止或退出 App 前，將瀏覽器插件切回直連。

macOS／Windows 的系統確認與完整遊戲流程須在實際裝置驗收，封裝成功不能代替這些結果。詳細操作見[客戶端操作](client.md)。

## 常見問題

| 現象 | 檢查方式 |
| --- | --- |
| 找不到 Go | 用 `go version` 檢查，或透過 `--go` 指定絕對路徑 |
| Release already exists | 使用新的 `--output-dir`，同步修改拓撲的 `releaseRoot` |
| 找不到公開 profile | 確認部署成功、`outputDir` 正確；需要時執行 `--export-client` |
| profile 驗證失敗 | JSON 只允許 `deploymentId`、`url`、`caCertificateFile`；公開憑證須在配置目錄內，且只能有一張 PEM 憑證 |
| 加速停用或沒有授權入口 | 確認建置及封裝時都設定同一個 `GPR_CLIENT_CONFIG`，重新建置後再安裝 |
| 授權成功卻沒有節點 | 在 Control 確認節點開放、健康且帳號已取得該節點權限 |

要刻意建置通用版時，macOS 使用 `unset GPR_CLIENT_CONFIG`，PowerShell 使用 `Remove-Item Env:GPR_CLIENT_CONFIG -ErrorAction SilentlyContinue`，再執行相同建置流程。
