# 驗證紀錄

## 驗證範圍

0.3.0 的測試以隔離環境驗證直連／上游代理、本機快取、素材預取、記憶體預熱、體檢、PAC、設定、介面及原生常駐。自建加速、Control 與 Gateway 不在本版本驗證範圍。

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

## Windows CI

[Windows 工作流程](../.github/workflows/check.yml) 在 `windows-2025` 執行鎖定依賴安裝、前端建置、Rust 格式／靜態檢查、核心與桌面單元測試、PAC 檢查、Chromium 互動及布局測試、原生 EXE 建置，以及 ZIP／SHA-256 完整性驗證。

CI 產物依賴系統 WebView2 Runtime，未做 Authenticode 簽署。自動測試成功不等於 Windows 實際 WebView2 互動、憑證信任、托盤或完整遊戲流程已驗收。工作流程須在推送至 GitHub 或手動觸發後才能取得實際執行結果。

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
