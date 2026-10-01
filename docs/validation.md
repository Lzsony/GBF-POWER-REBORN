# 驗證紀錄

## 驗證範圍

0.4.0 的驗證範圍包含直連／上游代理、本機快取、授權與 SSH 加速、Control／Gateway、公開 profile 建置及部署工具。各版本的實際完成結果分節列出；早期版本的原生或套件結果不等於本版本已重跑。正式主機部署、跨機網路與完整遊戲操作須單獨驗收。

## 2026-10-01：iPhone 真機驗收（基礎功能已驗收）

裝置為 iPhone 17 Pro Max／iOS 27.0.1。使用付費 Developer Team 為 App 及 VPN 擴充套件簽署；逐一核對 Network Extension、App Group、Keychain 群組、裝置描述檔與內嵌公開服務標記。未讀取或保存授權碼、私鑰、遊戲回應內容，也未產生截圖、錄影或視覺 trace。

| 項目 | 結果與證據範圍 |
| --- | --- |
| 簽署、安裝及啟動 | 通過：裝置註冊與開發者模式完成，乾淨建置的 Debug App／擴充套件簽章有效，CoreDevice 安裝成功並確認 GPR 程序執行。另完成 Release 簽署建置，但手機本輪運行 Debug 版本 |
| Control 入口 | 通過：Mac 使用公開配置的信任憑證驗證 TLS，健康入口 HTTP 200 |
| 授權及節點 | 使用者真機回報：授權成功、1 個可用節點、連線後節點延遲 60 ms；CoreDevice 同時確認 GPR 與 GPRTunnel 執行 |
| Chrome 純加速 | 使用者回報：遊戲正常載入，GPR 請求數增加、流量有變化 |
| CA 與 HTTPS 快取 | 使用者回報：描述檔已安裝並完整信任，GPR 系統信任檢查通過；開啟快取後 Chrome 正常、無憑證異常，命中率 0.4% |
| 網路切換 | 使用者回報：Wi-Fi 與行動數據兩次切換後均正常 |
| 背景及鎖屏恢復 | 使用者外出後回報「一切正常」，未提供精確持續時間或最後延遲。測試前後 GPR／GPRTunnel PID 由 713／937 變為 2146／2147，使用者確認曾手動重開 App／VPN，不據此判定閃退；不能把兩次檢查的時間間隔當作單一程序連續運行時間 |
| 診斷紀錄 | 恢復驗收、偵錯量測前的查詢未找到符合 GPR 名稱的崩潰檔及當日 JetsamEvent 檔；此為查詢結果，不等同證明完全沒有系統回收 |
| 記憶體 | 擴充套件自行呼叫 `task_info` 回報 physical footprint；新版已簽署、安裝及啟動。使用者在幾分鐘遊戲操作後回報目前 5 MB、目前程序峰值 8.8 MB，操作正常。這不代表已結束程序或長時間壓力測試的峰值 |
| 專項與合併 | IPv6／HTTP/3 繞過路徑、低記憶體／磁碟不足的真機專項尚未完成；維持 `ios-dev`，未合併 |

首次啟動曾被 iOS 的開發者線上驗證阻擋，使用者切換行動數據後即可開啟。這是系統啟動前的驗證結果，不等同 GPR 服務授權。另發現更新 provisioning profile 後增量產品可能殘留舊擴充套件簽章，已修正簽署入口先清理產品並獨立驗證內嵌 appex；封裝檢查 3 項及 Debug／Release 真機簽署建置通過。

外部 LLDB 附加沒有取得有效記憶體數值，其後原 VPN 程序不再執行，未將此當作外出期間閃退證據。已停止使用外部附加方式，改由 provider 在狀態回覆附帶用量，再以新安裝的 Debug 版驗收。最終 CoreDevice 確認 App／VPN 程序均運行；新增欄位的真機簽署建置、模擬器 Debug 建置及封裝檢查 3 項通過。版本仍為同批 0.5.0，未提交或合併。

證據位於 `artifacts/ios/device-acceptance-2026-10-01/`。CoreDevice 不允許讀取共享容器根目錄的 `GPR/config.json`，未使用其他路徑繞過此限制；功能數字明確標示為使用者回報。

## 2026-10-01：iOS 節點延遲檢查與快取簡化

已切回 `ios-dev`，延續未提交的 0.5.0 批次，保留既有 staged／unstaged 內容與 Xcode 工程調整。新增主介面連線後的節點 TCP 延遲檢查，取消 iOS 磁碟總容量配額與容量設定，移除檢查修復入口及橋接命令；RAM／單物件限制、手動清除與桌面快取契約保留。

| 項目 | 結果 |
| --- | --- |
| Rust 核心 | 128 項通過、3 項既有 opt-in／fixture 測試未啟用；包含實際本機 TCP 三次取樣、至少兩次成功的中位數、重複請求、取消、重連清除與未連線拒絕 |
| iOS C ABI | 4 項通過；移除的 audit／容量輸入在接觸核心或網路前拒絕，舊設定保持相容 |
| Swift | 11 項通過；包含授權回歸、檢查重入拒絕、同節點重連後舊結果隔離、停止清除及失敗不顯示為 0 ms |
| 快取 | 無配額保存／手動清除、有限配額淘汰、寫入失敗保留可用 RAM 回應與釋放佇列、超過 256 筆的批次清除均通過 |
| 靜態檢查 | workspace Clippy `-D warnings`、Rust 格式與差異空白檢查通過 |
| iOS 建置 | 更新 Rust XCFramework，arm64 真機目標與模擬器 Debug 無簽署建置通過，含 profile／版本與檔案雜湊檢查 |
| 真機範圍 | 本輪未執行 iPhone 安裝、正式節點延遲、Chrome 導流、CA 或背景測試；不以本機 TCP 測試代替真機驗收 |

輸出位於 `artifacts/ios/latency-cache-*` 與 `manifest-debug.json`。TCP 檢查只接觸本機測試節點，不取得遊戲內容；未產生截圖、錄影或視覺 trace。本輪不提交、不推送、不合併。

## 2026-09-25：0.5.0 iOS 可行性驗證工程

分支 `ios-dev`，以 Xcode 27.0／iOS SDK 27.0、Rust 1.98.1，在 macOS 上完成無簽署建置。最低 iOS 版本為 17；使用未配置服務的通用驗證建置，沒有連接正式 Control／Gateway、建立正式授權或更改系統 CA。

| 項目 | 結果 |
| --- | --- |
| Rust 核心與 iOS C ABI | 通過：`gbf-core` 121 項、`gbf-ios` 3 項；3 項原有 opt-in／子程序 fixture 測試未啟用 |
| 授權入口狀態機 | 通過：5 項 Swift 測試，包含線上驗證門檻、離線、撤銷後延遲成功、舊回覆、缺少 profile 及有效帳號無節點 |
| 內嵌 SSH | 通過：本機真實 SSH 握手、Ed25519、目的地網域保存、並行通道、不透明位元組、停止釋放及錯誤 host key 拒絕；未使用正式節點 |
| iOS 建置 | 通過：arm64 真機及 arm64 模擬器的 Rust XCFramework、SwiftUI App 與 Packet Tunnel 擴充套件，均為無簽署輸出 |
| 封裝輸入 | 通過：3 項核心版本／profile 混用拒絕測試、4 項公開 profile 測試；二進位標記與公開配置一致性檢查 |
| 依賴聲明 | 收集 331 項解析的 iOS Rust 依賴聲明，隨 App 包含；新增上游固定提交的授權全文與 SHA-256 |
| 桌面及共用回歸 | 通過：桌面 Tauri 編譯、前端建置、PAC 81 個案例 × 2 個埠、workspace Clippy `-D warnings` 與格式檢查 |
| iPhone 簽署／安裝 | 未執行：尚未提供付費 Developer Team 與 provisioning profile |
| Chrome／VPN／CA／背景 | 未驗證：需完成 [iOS 真機門檻](ios.md#真機門檻與合併條件)，無簽署編譯及桌面測試不能代替 |
| 服務端、Windows／macOS 正式封裝 | 本輪未重新部署或交付；不沿用歷史紀錄宣稱 0.5.0 已實測 |

驗證輸出位於 `artifacts/ios/`，包括 `manifest.json`、建置及測試紀錄，不納入 Git。未產生截圖、錄影或視覺 trace。

目前快取為原型配額：單物件 512 KiB、RAM 4 MiB、排隊寫入 2 MiB、解壓 2 MiB；大物件及 Brotli 回應串流轉發但不寫入快取。大物件磁碟串流快取、原生體檢進度／取消與正式介面驗收仍待真機可行性門檻通過，不能稱為整份 iOS 方案已完成。

### 同日修訂：Xcode Run 的模擬器連結失敗

Xcode 預設 Debug 建置嘗試同時連結 arm64 與 x86_64，而 XCFramework 只有 arm64，導致 x86_64 的 `_gpr_core_call`／`_gpr_core_free` undefined symbol。已在既有 Xcode 建置紀錄及不覆蓋架構的 iPhone 18 Pro 模擬器命令重現；先前腳本的 `ARCHS=arm64` 覆蓋掩蓋了工程缺少架構限制。

工程 `Base.xcconfig` 明確設定 arm64，腳本移除命令列架構覆蓋，加入 Debug 建置與 debug dylib 的 profile／版本檢查。修正後真機與模擬器無簽署 Debug 建置均通過；同一 iPhone 18 Pro／iOS 27.0 模擬器以預設設定完成 Debug 建置及模擬器 ad-hoc 簽署。封裝檢查 3 項仍通過。未執行真機安裝、VPN 或遊戲驗收；同批修訂維持 0.5.0。

### 同日修訂：主介面先顯示，再驗證授權

依更新後流程，主介面不再以授權狀態作為顯示條件。冷啟動先呈現主介面，再非阻塞檢查已保存身份；授權面板可從主介面開啟或關閉，驗證成功後關閉面板。未授權時啟動按鈕導向授權面板，只有授權有效且存在節點才允許啟動加速；撤銷、離線、缺少 profile 均保留主介面。VPN 擴充套件的獨立線上驗證不變。

通過 7 項 Swift 測試，包含節點與授權雙重啟動條件、撤銷／延遲回覆，以及驗證失敗時禁止受保護操作但仍允許停止；真機與模擬器無簽署 Debug 建置通過。未產生截圖、錄影或 trace，原生介面互動及真機 VPN／Chrome 尚待驗收。同批變更維持 0.5.0。

## 執行入口

```sh
npm ci
npm run build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm run test:pac
npx playwright install chromium webkit
npm run test:ui
npm run test:ui -- --browser=webkit
```

macOS 原生驗收使用獨立資料及識別：

```sh
cargo build -p gbf-power-reborn --features internal-test,tauri/custom-protocol --locked
node scripts/test-macos-native.mjs target/debug/gbf-power-reborn
cargo run -p gbf-power-reborn --example native_lifecycle --locked
cargo test -p gbf-core native_certificate_and_credential_round_trip --locked -- --ignored
```

CA／Keychain 測試可能要求系統確認。只有完成安裝、使用、移除與清理的操作才能記錄為通過。介面驗證不建立截圖、錄影或 trace。

## 服務端與部署檢查入口

```sh
cd server
go test ./...
go test -race ./...
go vet ./...
cd ..
python3 -m unittest discover -s deploy -p 'test_*.py'
python3 scripts/build-server.py --go go --arch all
```

Go 工具鏈依 `server/go.mod`；測試建立本機 Control／SSH fixtures 與臨時 SQLite，不使用正式主機。部署單元測試採離線替身驗證拓撲、命令與回復；它不能證明 systemd、雲端防火牆或遠端主機已配置成功。

客戶端通用版與內嵌服務 profile 的建置分別驗證，輸入格式與命令見[部署與建置](self-hosted-gateway.md)。封裝檢查內嵌標記與指定公開 profile 相符，排除加入 ticket、私鑰、授權碼及本機部署 journal。

## Windows CI

[Windows 工作流程](../.github/workflows/check.yml) 在 `windows-2025` 執行鎖定依賴安裝、前端建置、Rust 格式／靜態檢查、核心與桌面單元測試、PAC 檢查、Chromium 互動及布局測試、原生 EXE 建置，以及 ZIP／SHA-256 完整性驗證。

CI 產物依賴系統 WebView2 Runtime，未做 Authenticode 簽署。自動測試成功不等於 Windows 實際 WebView2 互動、憑證信任、托盤或完整遊戲流程已驗收。工作流程須在推送至 GitHub 或手動觸發後才能取得實際執行結果。

2026-09-17 的 [`main` 執行 #10](https://github.com/Lzsony/GBF-POWER-REBORN/actions/runs/35833181858) 在介面測試停下：75 項通過，語言／主題保存測試 1 項失敗，原生建置與封裝未執行。測試以固定 200 毫秒延遲觀察保存中的停用狀態，Windows CI 執行時該狀態可能在斷言前結束；改為由測試明確暫停並釋放第一次保存。修正後，本機受影響測試連跑 5 次、完整 76 項以 8 個及 1 個 worker 各跑一次均通過；這是本機驗證，GitHub 尚未執行修正版。較早的 #9 因當時缺少 `SshNotConfigured` 編譯失敗，已由後續版本修正，不是 #10 的失敗原因。

## Windows 本機驗收（2026-09-17，0.4.0）

使用 `main` 的 `87e7093` 與管理機匯出的公開 `client-build.local.json`／`control-public.crt` 建置自建版。公開 profile 檢查通過，未將授權碼、私鑰或整份拓撲放入建置材料。此 ZIP 依賴系統 WebView2 Runtime；本輪不含固定 Runtime，也未簽署。驗收腳本的修正只影響測試，產品版本維持 0.4.0。

| 項目 | 結果與證據 |
| --- | --- |
| 建置環境 | 通過：Node 24.19、npm 11.17、Rust 1.98.1、MSVC、Windows SDK 10.0.28000、系統 WebView2 153.0.4234.48。Python 3.12.14 使用本機依賴執行環境；初始 `python` 不在 PATH。 |
| 前端／Rust／PAC／介面 | 通過：`npm ci`、`npm run build`、`cargo fmt`、`cargo clippy`、工作區測試、PAC 兩組各 81 案、介面 76 案。工作區核心 123 通過／3 忽略，Windows process 2 通過／2 忽略，桌面 9 通過；憑證與憑證管理器的 opt-in 實機測試另行通過並清理測試項目。 |
| 原生操作 | 通過：內部版真實 WebView2 的跨路徑單實例、設定保存、PAC、代理啟停及埠釋放；原生生命週期驗證固定視窗、語言選單、托盤隱藏／恢復、隱藏後代理繼續運作、自動啟動項目的建立／清理。內部版使用隔離資料目錄。 |
| 正式 ZIP | 通過：x64 PE、0.4.0 版本、公開 profile 內嵌標記、manifest 與逐檔雜湊；`GBF-POWER-REBORN-0.4.0-windows-x64.zip` SHA-256 為 `53381cbf4472c4d1057f51abf10b970cb26ee6298831577d51e0d9bc245c901b`。解壓後移動目錄，從其他工作目錄啟動；正式 App 的 WebView2 子程序使用系統 Runtime。 |
| 既有設定 | 首次啟動因舊 `schemaVersion: 2` 失敗；目前 0.4.0 只接受版本 1。使用者明確決定不保留版本 2 設定，並自行移除舊 App 資料後重啟；新設定為版本 1，正式 App 啟動成功。 |
| 自建服務與授權 | 通過：公開憑證釘選的 Control `/health/live` 回應 200；使用者於正式 App 完成授權，回報授權有效且有 1 個節點，啟動後回報已連線。未保存授權碼。 |
| PAC／HTTPS／本機 CA | 通過：正式 App 的 PAC 回應 200 且包含本機代理規則；透過代理對兩個公開遊戲站點執行 HTTPS HEAD 均回應 200。使用者回報憑證已信任、快取已開啟及加速已連線；新 CA 指紋 `622D37DD50757C16BD2108345A296F547241CDA4` 已在 Windows CurrentUser Root，設定檔快取開關為 `true`。 |
| 瀏覽器遊戲操作 | 使用者回報：ZeroOmega 使用正式 PAC 後，登入及頁面操作正常，App 代理流量增加；此為使用者實機回報。 |
| HTTPS 靜態快取命中 | 當日未確認：使用者重開頁面後，App 命中數／命中率沒有增加。快取索引已有 446 筆 GET 資產，其中多筆帶長期快取標記；瀏覽器本身可能未再次向代理請求同一資產，不能據此判定代理快取實作故障。後續實機補測見 2026-09-18 紀錄。 |
| 快取索引資產補測 | 當日未執行：自動審批拒絕從索引取出未逐一確認的 URL 後再次向外部站點請求完整資產，理由是該請求的目的地與內容未獲具體授權。2026-09-18 取得使用者對單一公開資產的明確授權後完成補測。 |
| 收尾 | 通過：使用者回報 ZeroOmega 已切回直連且一般網頁可開啟，正式 App 的代理已停止；本機確認 8123 無監聽。App 視窗仍開啟，版本 1 設定、授權、快取開關及新 CA 信任均保留；隔離測試的自動啟動項目已清理。 |

ZIP 與完整性資料保存在本機忽略目錄 `artifacts/windows/`；隔離測試資料已清理。上述遊戲站點檢查只記錄 HTTP 狀態，不保存遊戲回應內容、截圖、錄影或視覺 trace。正式遊戲登入與頁面操作由使用者在本機完成，結果須標記為使用者回報；未對正式服務執行部署或改動。

## Windows 快取命中診斷（2026-09-18，0.4.0）

在正式 App 資料目錄唯讀彙總快取索引，不輸出資產內容：共 1,067 筆已保存的 GET／200 資產，591 筆沒有明確 `max-age`／`Expires`，476 筆宣告 `max-age=31536000`；後者有 198 筆以預取使用的 `Accept-Encoding: identity` 保存。快取鍵包含完整 URL；瀏覽器若直接使用自身快取、不再送出相同 URL，或送出帶條件標頭／不同編碼的請求，App 不會計為本機新鮮命中。這些是可能原因，無法還原前一天瀏覽器每筆請求。

本機隔離的 HTTPS 代理測試通過：日文資產 2 次、英文資產 6 次請求，對本機上游各取回 1 次，計入 6 次快取命中；304 重新驗證測試亦通過。為取得具體授權，先向使用者展示一個從索引選出的無 query、無憑證參數公開 JS URL；使用者核准後，正式 App 代理對該資產兩次 GET 均回應 200、各 379 bytes；原快取回應時間保持不變，索引存取時間更新。使用者回報 App 此時顯示下載 65、命中率 2.9%，確認正式 Windows App 出現實際本機命中。回應內容未保存或輸出。

Python 使用系統預設信任時發生憑證簽章驗證錯誤；系統信任庫中有同名舊 CA，但尚未證明它是唯一原因。改用目前 App 的公開 `local-ca.pem` 明確驗證後成功，沒有關閉 TLS 驗證。此結果不代表能確定前一天瀏覽器未命中的單一原因，也未變更產品程式碼或版本。收尾時再次確認本機 8123 無監聽，版本 1 設定、快取開關、授權與新 CA 信任仍在。

## 本機套件

`npm run package:macos` 只在 Apple Silicon 上建置，輸出 App、DMG、SHA-256 與逐檔 manifest 至 `artifacts/macos/`。檢查識別、版本、arm64 架構、macOS 13.0 最低版本宣告、授權文件、ad-hoc 簽章、測試入口排除及 DMG 唯讀掛載內容一致性。

套件未經 Developer ID 簽署或 Apple 公證；manifest 的 `sourceFiles` 描述建置內容，`baseCommit` 僅為當時 HEAD，不代表尚未提交的原始碼已納入該 commit。

## 本機結果（2026-09-16）

環境為 macOS 27、Apple Silicon；以下檢查均在本專案執行。

| 檢查 | 結果 |
| --- | --- |
| 前端 TypeScript／Vite 建置 | 通過 |
| Rust 格式與 Clippy（workspace、all-targets） | 通過 |
| 核心單元／隔離網路測試 | 86 通過、0 失敗、2 項預設忽略 |
| 桌面單元測試 | 7 通過 |
| PAC 規則 | 81 組案例 × 2 個埠號通過 |
| Chromium 介面測試 | 42 通過 |
| WebKit 介面測試 | 42 通過 |
| 真實 WKWebView | 啟動錯誤、全新設定及重啟共 3 組情境通過 |
| 原生生命週期 | 6 項檢查通過，含托盤、隱藏、連線保留及停止釋放埠號 |
| 隔離 CA／Keychain | 另行 opt-in 執行 1 項通過，完成測試材料清理 |
| macOS arm64 release build | 通過 |
| App／DMG 封裝 | arm64、版本／識別、授權文件、ad-hoc 簽章及唯讀掛載內容一致性通過 |
| Windows CI | 首次執行因提交缺少兩個快取模組而在 Rust 格式檢查停止；修正待重跑 |

核心預設忽略項目分別為受控子程序替身與 opt-in CA／Keychain 測試；前者由命令測試啟動，後者已另行驗證。介面測試使用模擬 IPC，WKWebView 與原生生命週期另行驗證實際整合。

詳細輸出保存在本機 `artifacts/validation/`；套件完整性以封裝入口成功結果及 `artifacts/macos/manifest.json` 為準。未驗證 Windows 真機、最低 macOS 版本及完整遊戲操作；macOS Intel 不在本版本範圍。

## 完整快取驗證（2026-09-17）

本輪新增素材預取、記憶體預熱、可取消體檢、背景排程、偏好保存及其介面。功能版本更新為 0.2.0，schema 1 缺少快取偏好時套用預設值，既有設定不重設。

| 檢查 | 結果 |
| --- | --- |
| 核心單元／隔離網路測試 | 101 通過、0 失敗、2 項預設忽略 |
| stage 匯出的乾淨快照 | Rust 格式與核心 all-targets 編譯通過；新增模組已納入 Git |
| 桌面單元測試 | 7 通過 |
| 前端建置、Rust 格式與 Clippy | 通過 |
| Chromium／WebKit 介面測試 | 各 53 通過 |
| 真實 WKWebView | 啟動錯誤、全新設定及重啟共 3 組情境通過 |
| Windows 隔離 runner | 已更新快取操作斷言，僅驗證腳本語法；本輪未在 Windows 執行 |

核心覆蓋預取白名單、同語言限制、佇列合併、前台讓出、取消後不保存未完成下載，以及預取不污染瀏覽器請求／命中率。預熱檢查有效期、容量、並行替換與 LRU／統計不變；體檢檢查損壞修復、過期有效項目保留、取消及檔案邊界。

介面與原生檢查涵蓋雙開關 patch 保存、失敗回復、代理 URL 草稿保留、體檢進度／失敗／取消、操作互斥、隱藏／恢復及體檢中退出。原生測試只使用獨立識別與臨時資料，未修改正式 CA 或應用資料。

原生 internal-test 建置需先完成前端建置，並確認嵌入資產已更新；首次測試遇到空嵌入頁面時，重建該套件的 dev profile 後完整情境通過，未延長測試逾時。

本輪詳細結果保存在 `artifacts/validation/cache-*`；Mac 套件以封裝入口成功結果與 manifest 為準。核心的 CA／Keychain opt-in 測試未重跑，其結果仍以 2026-09-16 紀錄為準。Windows CI 與完整遊戲操作須另行驗證。

## 0.2.0 版本封裝

快取功能的完整回歸沿用上節結果；版本號調整另檢查 npm／Cargo／Tauri 與鎖定檔一致性、介面版本顯示及正式 App／DMG 的版本資訊。版本變更不改動 schema 1、應用識別、資料目錄或監聽埠。

## 四語介面驗證（2026-09-17，0.3.0）

新增日本語及 English，同步主介面、快取管理、體檢、錯誤訊息、托盤、macOS 選單及原生啟動提示。首次啟動跟隨系統語言；中文按地區及 Hans／Hant 選擇，日文使用 `ja`，其餘使用 `en`。已保存的語言偏好保留，schemaVersion 維持 1。

| 檢查 | 結果 |
| --- | --- |
| 翻譯完整性 | 四份字典各 142 個訊息鍵，內容非空且插值參數一致 |
| 語言與偏好相關 Rust 測試 | 5 項通過，涵蓋系統語言判定、序列化及偏好保存 |
| 桌面單元測試 | 8 項通過，包含四語原生提示及選單文字 |
| TypeScript／Vite、Rust 格式與 Clippy | 通過 |
| Chromium 介面 | 71 個案例通過；3 個預覽斷言修正後重跑通過，其餘 68 個已通過 |
| WebKit 介面 | 71 個案例通過 |
| 真實 WKWebView | 啟動錯誤、全新設定、English 重啟及日本語重啟共 4 組情境通過 |
| 原生生命週期 | 6 項檢查通過，包含四語托盤選單及運行中切換 |
| Windows 原生腳本 | 已更新四語操作及語言無關的 Runtime 檔名檢查，實際執行待驗收 |

介面覆蓋四語選擇、缺省語系回退、保存及錯誤翻譯，以及 400 × 520 和較小工作區的主頁、選單與體檢布局。原生重啟驗證確認 English／日本語不會被系統語言覆蓋；仍保留快取、代理、登入啟動、托盤及退出回歸。

初次偏好測試有一項受到 sandbox 的本機 listener 限制，在允許 loopback 的環境重跑後通過。原生腳本對已選語言重複點擊會與主題操作競爭，改為只在語言不同時點擊後，完整原生情境通過；未延長測試逾時。

結果保存在 `artifacts/validation/i18n-*`。只調整語言及版本相關行為，核心快取完整測試的 101 項結果仍以完整快取驗證紀錄為準。Windows CI、Windows 實機及完整遊戲操作未在本輪執行；不產生截圖、錄影或 trace。

## Control／Gateway 與自建服務驗證（2026-09-17，0.4.0）

本版本新增內嵌公開服務配置、授權隔離、SSH Gateway、自動／手動選線、Go Control／TUI 及 Debian／Ubuntu systemd 部署工具。協定版本為 1；客戶端設定 schemaVersion 為 1，Control SQLite user_version 為 2，兩者獨立管理。

| 檢查 | 已完成結果 |
| --- | --- |
| Rust 核心單元／隔離網路測試 | 118 通過、0 失敗、3 項預設忽略 |
| 桌面單元測試 | 8 通過；workspace 合計 126 通過 |
| Rust Clippy（workspace、all-targets） | 通過 |
| 前端建置、四語翻譯與 Chromium 互動／布局 | 建置通過；76 項介面測試通過 |
| PAC 共用規則 | 81 組案例 × 2 個埠號通過 |
| 公開 profile 建置與拒絕測試 | 通用／自建設定可編譯；CRLF 憑證、IPv6 位址、無效 DER 與多餘秘密欄位已核對；Python 4 項通過 |
| Go 服務端單元與隔離整合測試 | 20 通過 |
| Go race detector | 20 通過，無 race 報告 |
| Go vet、gofmt、建置版本注入 | 通過；本機 CLI 回覆 `0.4.0` |
| 部署離線／替身測試 | 30 通過 |
| Linux amd64／arm64 release | 交叉建置與 manifest 驗證通過；不是 Linux 主機運行驗收 |
| 0.4.0 真實 WKWebView | 首次啟動與兩次重啟共 3 組情境通過；測試專用體檢暫停標記已按階段清理 |
| macOS 原生生命週期 | 6 項檢查通過，包含四語選單、托盤啟停、視窗尺寸及監聽埠釋放 |
| macOS arm64 正式 App／DMG | 封裝入口驗證版本、識別、arm64、授權文件、ad-hoc 簽章、唯讀掛載內容、公開 profile 標記與逐檔 SHA-256 |
| 正式 Debian／Ubuntu systemd 與跨主機部署 | 未執行 |
| Windows CI／Windows 真機及完整遊戲流程 | 本輪未執行 |

核心檢查涵蓋公開 profile 欄位、按部署隔離的授權碼／裝置身份、服務信任變更重新授權、簽名 Control 請求、Gateway host-key pin、無配置時不發送 Control 請求，以及既有快取與代理回歸。

Go 測試涵蓋重播／簽名／角色拒絕、裝置額度、帳號 epoch 與撤銷、租約過期、受阻 Control 不阻塞過期處理、兩個隔離 Gateway 的轉發邊界、容量／過期上報、新帳號明確選節點，以及帳號和 grants 失敗時的交易回復。TUI 使用 Bubble Tea 模型及鍵盤事件測試，不宣稱已人工操作正式管理終端。

部署測試涵蓋管理 SSH 別名去重、單主機單 Gateway、同機角色分離、主機部署鎖二次驗證、release／秘密排除、加入失敗重跑、穩定部署識別、公開 profile 匯出及撤回。服務端身份、Control 資料庫與部署狀態只存在於臨時 fixture；未連接正式節點。

本輪證據位於本機 `artifacts/validation/acceleration-core-*`、`server-*`；Linux 產物位於 `artifacts/server/`。Go 1.27.0 工具鏈另核對官方 SHA-256，未安裝至全域。介面驗證不產生截圖、錄影或 trace。

## 單主機全新部署與 Mac 接入（2026-09-17，0.4.0）

本輪在已清理的 Debian 13 amd64 主機，以同機獨立 Control／Gateway 完成新版工具實機部署；未遷移舊帳號、資料庫或身份。管理 SSH、主機名稱解析與持久化防火牆另行準備，部署工具本身不修改防火牆。客戶端使用本次部署匯出的公開 profile，於 Apple Silicon Mac 正式打包、安裝及操作。

| 檢查 | 已完成結果 |
| --- | --- |
| Linux amd64 release | Go 1.27 建置及 manifest 驗證通過；部署版本 0.4.0 |
| 新版部署流程 | `--plan`、`--apply`、`--verify`、`--export-client` 均通過 |
| 原生 systemd 與外部入口 | Control／Gateway 運行且設為開機啟動；公開 Control TLS 與 Gateway SSH host key 驗證通過 |
| 主機網路 | nftables 持久化、TCP 22／443／2222 入站規則及套用後全新管理 SSH 連線通過 |
| 管理與授權 | 新節點健康後明確開放；新建帳號明確取得節點授權；TUI 帳號／節點頁可讀，服務端確認 1 台裝置在線及實際流量 |
| Mac 全新設定 | 兩代舊客戶端資料及專屬秘密項目已清除；按精確指紋移除舊 CA，獨立確認不再受信任後才刪除資料 |
| 正式 App／DMG | 封裝入口通過，包含內嵌 profile、版本、arm64、簽章、授權文件及 DMG 內容核對；已安裝至 Applications |
| 原生 App 操作 | 透過輔助使用元素完成繁體中文設定、授權碼啟用、加速連線、新 CA 安裝及快取啟用；系統獨立驗證新 CA 信任通過 |
| 瀏覽器 PAC | 現有 ZeroOmega GBF 情境更新至本機 8123 埠，重新下載 PAC 並選為目前情境 |
| 實際公開頁與靜態快取 | 加速代理公開頁回應 HTTP 200；Chrome 遊戲入口載入完成且無憑證錯誤，觀察到 75 個 CDN 靜態資源；具明確快取期限的公開 JS 兩次回應 HTTP 200、內容雜湊一致，App 出現本機快取命中 |

未提供新鮮期限的圖片會回源驗證，不將這類重複請求算成本機新鮮命中。驗收使用 HTTP 狀態、內容雜湊、原生介面文字、瀏覽器元素及服務端流量交叉核對；不保存截圖、錄影、視覺 trace、授權碼或遊戲回應內容作為證據。

使用者另回報已完成實際登入、切換頁面及平常操作，運作正常；此項為使用者實機驗收回報，不是自動化遊戲測試。本輪僅驗證一臺主機，未執行跨主機部署、主機重開機、失敗撤回演練或 Windows 驗收。未修改產品程式碼或協定，版本維持 0.4.0。私有拓撲、journal、套件及驗證證據保留在本機忽略目錄 `artifacts/gc-jp-redeploy/`，不納入版本庫。

## 新手指引與 gpr 管理入口（2026-09-17，0.4.0）

新增部署／建置指引及主機端 `gpr` 命令，併入尚未提交的 0.4.0 功能批次。入口透過既有 sudo 權限呼叫固定的系統工具與角色執行檔；API、設定 schema 與已交付 App／DMG 不變。

| 檢查 | 已完成結果 |
| --- | --- |
| Python 部署與管理測試 | 48 項通過，包含 18 項新增管理測試；覆蓋角色缺失、參數拒絕、權限不足、退出碼、日誌追蹤、啟停順序、安裝衝突、原子替換失敗及單獨安裝流程 |
| 語法與文件 | Python 編譯檢查、CLI help、55 個本機文件連結／錨點通過；兩份新手指引無真實節點資料 |
| 既有 Debian 13 主機補裝 | 僅執行 `--install-manager`；檔案為 root:root 0755，部署內容 SHA-256 與本機來源一致 |
| 重複安裝 | 回報 `changed: false`，不重新安裝或重啟服務 |
| 實機唯讀管理 | `gpr --help`、`gpr status`、有限行 `gpr logs gateway`、`gpr doctor` 成功；診斷中的未檢查項目保留原狀態 |
| TUI | 透過 `ssh -t` 開啟 `gpr admin`，可讀取既有帳號及在線裝置，按 `q` 正常退出 |
| 不中斷確認 | 安裝前後兩個服務的 PID、InvocationID、啟動時間及金鑰檔案擁有者／權限／修改時間一致 |

啟停、重啟與失敗情境採隔離測試；未為測試中斷正式節點。新版 `--apply` 的自動安裝入口已由離線測試覆蓋，本輪未重新部署正式服務。證據保存於本機忽略目錄 `artifacts/manager-*/`；未重建客戶端、修改 sudoers 或操作其他主機。

## 遠端依賴補齊（2026-09-17，0.4.0）

新增不依賴遠端 Python 的 shell 初始檢查、限定 APT 依賴準備及 `--install-deps`；`--plan` 維持唯讀，缺失項目與未完成檢查明確區分。部署與管理入口安裝可先補齊依賴，驗證、匯出與撤回不自動安裝。

| 檢查 | 已完成結果 |
| --- | --- |
| 部署／管理／依賴測試 | 68 項通過，包含 20 項新增依賴測試 |
| 真實 shell 與隔離 APT 替身 | 缺少 Python、多項依賴、CA 缺失、完整環境無 APT 操作、安裝後重查、已安裝但命令遺失、鎖占用及 APT 失敗通過 |
| 交易限制 | 模擬階段拒絕升級／移除；實際安裝 hook 拒絕模擬後才出現的既有套件改動，失敗不進入服務部署 |
| 管理流程 | 主機去重、release 先行檢查、無 release 的依賴安裝、唯讀模式不安裝、匯出僅檢查 Control、撤回僅檢查相關主機及獨立逾時設定通過 |
| 語法 | bootstrap shell 語法及 Python 編譯檢查通過 |
| Debian 13 正式主機 | 新版 `--plan` 唯讀檢查完成，依賴缺失清單為空；兩個服務的 PID、InvocationID 與啟動時間前後一致 |

套件失敗與缺失安裝使用隔離替身；本輪沒有在正式主機安裝／卸載套件，也未在全新 Debian／Ubuntu 主機執行真實 APT 安裝。遠端 15 分鐘逾時參數與錯誤處理由替身驗證，未實際等待 15 分鐘或終止真實 dpkg。網路參數、核心、sshd、防火牆及 sudoers 均未調整；既有 App／DMG 未重建。沿用未提交的 0.4.0 批次，API 與 schema 不變。證據位於本機忽略目錄 `artifacts/dependencies-*/`。
