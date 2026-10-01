# iOS 客戶端

## 狀態與範圍

0.5.0 新增 iPhone 可行性驗證工程，工作分支為 `ios-dev`。目標為在外部 Chrome 遊玩 GBF，提供授權節點加速與本機 HTTPS 素材快取。2026-10-01 已完成 iPhone 17 Pro Max／iOS 27.0.1 的開發簽署、安裝與啟動；使用者回報授權、Chrome 加速、CA 信任、快取命中、網路切換及背景使用正常，VPN 記憶體用量／目前程序峰值為 5 MB／8.8 MB。未中斷的連續長測及 IPv6／HTTP/3 等專項仍待補驗，詳細證據見[驗證紀錄](validation.md)；不合併回 `main`。

最低建置目標為 iOS 17，介面支援简体、繁體、日本語與 English。首版僅提供加速，不提供直連或使用者指定的上游代理。桌面客戶端維持原有三種模式。

Apple 的 [TN3120](https://developer.apple.com/documentation/technotes/tn3120-expected-use-cases-for-network-extension-packet-tunnel-providers) 將 Packet Tunnel 內承載本機代理列為非支援用途。本工程依已確認方案，先驗證 `NEProxySettings`／PAC 導流的可行性，不聲稱它是 Apple 保證支援或可上架的實作。若真機導流或背景運作失敗，須記錄證據並重新決定架構，不能直接宣稱功能完成。

## 授權入口

啟動流程為 `啟動 → 主介面 → 授權驗證`。主介面立即顯示，不依賴網路、授權狀態或服務配置；授權是加速功能的使用條件，不是主介面的進入條件。啟動後非阻塞檢查已保存身份，使用者可由主介面的「授權驗證」開啟可關閉的授權面板。未授權時點選啟動按鈕會先開啟該面板，不啟動 VPN。

| 狀態 | 行為 |
| --- | --- |
| checking | 主介面保持可見，授權區顯示驗證進度 |
| activationRequired | 主介面顯示未授權；授權面板提供輸入、貼上與驗證 |
| verificationUnavailable | 主介面顯示網路／服務錯誤，保留身份，授權面板可重試 |
| configurationError | 主介面顯示缺少內嵌 profile；加速不可啟用 |
| authorized | 授權面板返回主介面；存在可用節點時允許啟動加速 |

之後每次冷啟動使用已保存的裝置身份，自動向 Control 驗證，不要求重填授權碼；離線仍可查看主介面，但不能略過加速啟動驗證。每次驗證有世代編號，舊回覆不能覆蓋新驗證或撤銷結果。

運行中授權輪詢遇到短暫網路失敗不等同撤銷；Gateway 租約與既有授權時效仍生效。收到明確撤銷時停止加速，主介面保持顯示並更新未授權提示；回到前景時重新讀取授權狀態。解除裝置先停止 VPN，Control 確認成功才清除本機授權狀態，並停留在主介面。驗證中、驗證失敗或撤銷後仍允許停止既有 VPN；節點設定、快取維護與解除裝置等操作維持授權限制。

VPN 擴充套件獨立驗證授權及節點，因此從系統設定啟動 VPN 也不能繞過授權。App 與擴充套件都不能用本機布林值替代 Control 驗證。

## 元件與資料流

- `ios/`：SwiftUI App、授權入口、Network Extension、共享 Security.framework 介面及 Xcode 工程。
- `crates/gbf-ios/`：序列化 JSON C ABI、原生呼叫白名單、生命週期與跨程序排他鎖；由 `gbf-core` 提供授權、代理與快取。
- 共用核心的 iOS SSH 後端使用 `russh`，桌面仍使用外部 OpenSSH；兩者共享節點與狀態型別。Control v1、Ed25519、host-key pin、目的地白名單與禁止重播契約不變。

流量路徑為 `Chrome → VPN 的 PAC 設定 → 擴充套件內代理 → 內嵌 SSH direct-tcpip → Gateway → 原始服務`。PAC 未匹配的流量由系統原路連線；不接管全裝置封包、不實作 Fake-IP 或 UDP 轉發。是否存在 QUIC、IPv6 或其他瀏覽器繞過路徑，必須實機驗證。

SSH SOCKS 入口只接受白名單網域的 TCP 80／443，不接受裸 IP、UDP、BIND 或其他目的地。每條 SSH 連線上限 32 條並行轉發，視窗為 256 KiB。斷線關閉現有通道，新請求不改走直連；重連僅重建同節點傳輸，不重播遊戲內容。

App 使用短生命週期核心完成停止狀態下的操作；VPN 運行時由擴充套件持有核心。`ios-core.lock` 排他鎖避免跨程序同時修改設定或快取。VPN 運行時 App 透過 provider message 查詢；IPC 接受 `status`、`refresh`，以及節點檢查的 `startConnectionCheck`／`connectionCheckStatus`／`cancelConnectionCheck`。檢查命令只接受工作識別碼，不接受使用者指定主機；設定、授權碼及快取清除不經該入口。

App Groups 保存非秘密設定、公開 CA 與快取，排除 iCloud 備份。授權碼、裝置私鑰與 CA 私鑰保存在共享 Keychain，使用 `AfterFirstUnlockThisDeviceOnly`，不寫入 JSON 或交付包。裝置首次解鎖前無法取得秘密時啟動失敗，不降級繞過驗證。

Control 僅信任內嵌公開 profile 的憑證；iOS 素材來源使用 WebPKI 根憑證驗證，避免將桌面原生憑證目錄讀取方式套用到 iOS。瀏覽器端 CA 信任由 Security.framework 驗證，不能將 App 自產 CA 當作自動信任錨點。

## 連線後節點延遲檢查

主介面在 VPN 已連線時顯示「檢查連線」。只有授權有效、目前 SSH 已連線且綁定可用時才啟用；自動選線也只量測實際連線中的節點，不重跑選線。

檢查採 TCP connect，DNS 解析不計入延遲；DNS 最多 2 秒，連續 3 次取樣各最多 2 秒，背景工作總期限 9 秒。至少成功 2 次才顯示成功樣本的中位數，並顯示完成時間；失敗顯示錯誤狀態，不顯示為 0 ms。這是節點 TCP 延遲，不是遊戲 API 或整段 HTTPS 請求耗時。

一次只執行一項檢查，App 每 500 ms 查詢工作狀態。核心呼叫立即回覆工作識別碼，測量不占住 C ABI 序列化鎖，不改變節點、SSH 連線或遊戲流量統計。停止、撤銷、節點變更或同節點重連時清除結果；工作識別碼及 SSH 連線識別碼共同防止舊回覆覆蓋新連線。停止按鈕不因檢查而停用。

連線時，統計區另顯示 VPN 記憶體用量與峰值。擴充套件自行透過 `task_info(TASK_VM_INFO)` 讀取 physical footprint，在 `status`／`refresh` 回覆附上 `memory.physicalBytes`／`memory.peakPhysicalBytes`；不需要偵錯器附加，也不讀取遊戲資料。峰值屬於目前擴充套件程序，不能還原已結束程序的峰值；系統讀取失敗時不回傳此欄位。

## 快取與憑證

延用桌面靜態來源／路徑白名單、HTTP 快取語意、日英分區及內容完整性驗證；API 與登入流量保持端到端 TLS。預設關閉 HTTPS 快取，iOS 磁碟快取不設總容量配額，不提供容量設定；RAM 仍限制為 4 MiB，停用預取與預熱。磁碟寫入失敗只使本次保存失敗，不讓已取得的正常回應變成請求失敗；不因總容量自動刪除舊快取。

為進行首階段記憶體可行性驗證，目前單物件快取上限為 **512 KiB**、排隊寫入上限為 2 MiB、並行快取下載為 2、解壓驗證上限為 2 MiB。較大物件及 Brotli 編碼回應正常轉發但不加入快取。這是原型限制，不是已完成原方案的大物件磁碟串流快取；該功能與配額調整須在實機可行性門檻通過後完成。

App 只匯出含公開 CA 的 `.mobileconfig`。使用者須安裝描述檔，再到「設定 → 一般 → 關於本機 → 憑證信任設定」開啟完整信任。系統信任檢查與 Chrome 的真實 TLS／快取命中分開驗證；刪除 App 不會移除系統內已安裝 CA。說明依 [Apple 憑證信任文件](https://support.apple.com/en-us/102390)。

保留「清除快取」，限 VPN 停止時執行。清除以固定批次讀取索引，避免無上限磁碟快取導致一次載入整份索引。iOS 已移除檢查修復的介面及橋接命令；桌面容量設定、LRU 容量淘汰與體檢不變。

共用快取 API 使用可選配額，iOS 傳入 `None`，桌面傳入設定容量。iOS 的保存輸入及狀態輸出不再含 `cacheLimitGb`；共用 schema 中的既有欄位保留相容性，但不影響 iOS 配額，不清除既有素材、不提高 schemaVersion。

## 建置與安裝

需要 macOS、完整 Xcode、Python 3、Rust 及 iOS 標準函式庫。支援 Apple Silicon 真機／模擬器建置；使用者自己的付費 Apple Developer Team、App Groups 與 Network Extension provisioning profile 是安裝驗收條件。免費 Personal Team 不足以簽署此擴充套件。

```sh
# 工程已納入版本庫；只有來源清單變更才需要重新生成。
python3 scripts/generate-ios-project.py

# 無簽署編譯與核心封裝，不代表能安裝到 iPhone。
python3 scripts/build-ios.py --unsigned

# 授權入口狀態測試，不啟動 VPN。
npm run test:ios
```

建置入口產生 `artifacts/ios/GPRCore.xcframework`、兩個平台的 App、依賴授權及 hash manifest。Homebrew Rust 缺少 iOS 標準庫但已附 `rust-src` 時，腳本以同版編譯器和 `build-std` 建置；否則要求先補齊目標或原始碼。`--platform device`／`simulator` 可限定平台，`--core-only` 僅建置核心。

目前 XCFramework 的真機與模擬器均只包含 arm64；Xcode 工程的 `ARCHS` 同樣固定為 arm64，不建置 Intel x86_64 模擬器。建置腳本不再透過命令列覆蓋架構，確保檢查與 Xcode「Run」使用相同工程設定。`--configuration Debug` 可驗證 Run 的 Debug 建置；Xcode 啟用 debug dylib 時，腳本在實際承載核心的 dylib 驗證 profile／版本，並輸出 `manifest-debug.json`。

自用版本沿用[公開 profile 建置格式](build-guide.md)，先設定 `GPR_CLIENT_CONFIG` 再重新建置 Rust 核心。profile 在編譯時內嵌，不能只在 Xcode 變更環境後重用舊 XCFramework。

```sh
export GPR_CLIENT_CONFIG=/absolute/path/client-build.local.json
python3 scripts/build-ios.py --unsigned
```

複製 `ios/Config/Local.example.xcconfig` 為 `Local.xcconfig`，填寫 `DEVELOPMENT_TEAM` 與自己 Team 可使用的 App／App Group 識別。Xcode 開啟 `ios/GPR.xcodeproj`，使用 `GPR` scheme，確認 App 與 `GPRTunnel` 都包含 Network Extension、同一 App Group 與共享 Keychain 群組；連接 iPhone 並完成開發者模式、簽署與安裝。

命令列簽署建置使用 `python3 scripts/build-ios.py --platform device --team YOUR_TEAM_ID`。腳本不自動註冊開發者帳號、建立公開服務、不發佈測試版，也不將秘密提交到版本庫。若使用 `--skip-core`，仍核對 XCFramework 與 App 的公開 profile 標記。

簽署建置會先清理目前配置的產品目錄，避免更新 provisioning profile 後沿用舊擴充套件簽章，並分別驗證 App 及內嵌 `GPRTunnel.appex`。新增真機須先由 Xcode 將裝置加入開發描述檔；只做通用真機建置不代表該描述檔已包含新裝置。若 iPhone 顯示需要網際網路驗證開發者，屬於 App 啟動前的系統驗證；2026-10-01 曾在切換行動數據後通過，不能據此判定 GPR 授權或 VPN 已可用。

## 真機門檻與合併條件

記錄 iPhone 型號、iOS／Chrome 版本、建置 profile 標記及測試日期，完成以下項目才可將 iOS 功能視為可用：

1. 啟動直接顯示主介面、授權面板開啟／關閉、首次授權、重啟非阻塞驗證、離線重試、無節點、撤銷、解除裝置及系統 VPN 啟動入口。
2. 連線後節點延遲檢查、斷線／重連結果清除，以及 Chrome 登入、遊戲操作、IPv4／IPv6、HTTP/3 與 PAC 範圍；以本機及 Gateway 計數確認目標流量未繞過。
3. CA 安裝與完整信任、Chrome TLS、同一可快取素材首次回源及再次命中。
4. App 退到背景遊玩至少 30 分鐘、鎖屏恢復、Wi-Fi／行動網路切換、斷網重連、停止後網路恢復。
5. 擴充套件記憶體峰值、低記憶體／磁碟不足／異常終止恢復，確認大物件快取的可用配額與後續實作。

目前以功能原型供上述門檻驗證；正式介面與節點延遲的真機驗收、大物件快取仍待門檻通過。不以桌面回歸、無簽署 App 或模擬器編譯結果替代真機測試，不產生截圖、錄影或視覺 trace。分支須保留至開發及必要測試通過後再合併。
