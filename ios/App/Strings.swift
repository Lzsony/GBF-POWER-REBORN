import Foundation

enum Strings {
    private static let values: [String: [String]] = [
        "checkConnection": ["檢查連線", "检查连接", "接続を確認", "Check connection"],
        "checkingConnection": ["正在測量節點延遲…", "正在测量节点延迟…", "ノード遅延を測定中…", "Measuring node latency…"],
        "nodeLatency": ["節點延遲", "节点延迟", "ノード遅延", "Node latency"],
        "checkedAt": ["檢查時間", "检查时间", "確認時刻", "Checked at"],
        "vpnMemory": ["VPN 記憶體用量", "VPN 内存用量", "VPN メモリ使用量", "VPN memory"],
        "vpnMemoryPeak": ["本次 VPN 記憶體峰值", "本次 VPN 内存峰值", "今回の VPN メモリピーク", "VPN session peak memory"],
        "connectionCheckFailed": ["節點延遲檢查失敗，請重試。", "节点延迟检查失败，请重试。", "ノード遅延を測定できませんでした。再試行してください。", "Node latency check failed. Please retry."],
        "authorization": ["授權", "授权", "認証", "Authorization"],
        "authorizationDescription": ["輸入授權碼並完成驗證後，即可啟用加速。", "输入授权码并完成验证后，即可启用加速。", "認証コードを確認すると高速化を利用できます。", "Verify your access code to enable acceleration."],
        "authorized": ["授權有效", "授权有效", "認証済み", "Authorized"],
        "authorizationRequired": ["尚未取得有效授權", "尚未取得有效授权", "認証が必要です", "Authorization required"],
        "verifyAuthorization": ["授權驗證", "授权验证", "認証を確認", "Verify authorization"],
        "authorizeToStart": ["授權後啟動加速", "授权后启动加速", "認証して高速化", "Authorize to accelerate"],
        "verifying": ["正在驗證授權…", "正在验证授权…", "認証を確認中…", "Verifying authorization…"],
        "accessCode": ["授權碼", "授权码", "認証コード", "Access code"],
        "activate": ["驗證授權碼", "验证授权码", "コードを認証", "Verify access code"],
        "verificationUnavailable": ["暫時無法驗證，請確認網路後重試。", "暂时无法验证，请确认网络后重试。", "認証できません。接続を確認して再試行してください。", "Verification is unavailable. Check your connection and retry."],
        "configurationError": ["此版本未包含有效服務配置，請重新建置。", "此版本未包含有效服务配置，请重新构建。", "サービス設定がありません。再ビルドしてください。", "This build has no valid service profile. Rebuild with a profile."],
        "experimental": ["iOS 可行性驗證版；外部瀏覽器導流與背景快取仍須真機驗收。", "iOS 可行性验证版；外部浏览器导流与后台缓存仍须真机验收。", "iOS 検証版。ブラウザ転送とバックグラウンドキャッシュは実機検証が必要です。", "iOS prototype. Browser routing and background caching still require device validation."],
        "retry": ["重試", "重试", "再試行", "Retry"],
        "connected": ["已連線", "已连接", "接続済み", "Connected"],
        "connecting": ["連線狀態變更中", "连接状态变更中", "接続状態を変更中", "Connection changing"],
        "stopped": ["已停止", "已停止", "停止中", "Stopped"],
        "start": ["啟動加速", "启动加速", "高速化を開始", "Start acceleration"],
        "stop": ["停止加速", "停止加速", "停止", "Stop acceleration"],
        "noNodes": ["暫無可用節點", "暂无可用节点", "利用可能なノードがありません", "No available nodes"],
        "statistics": ["統計", "统计", "統計", "Statistics"],
        "requests": ["請求", "请求", "リクエスト", "Requests"],
        "received": ["下載流量", "下载流量", "受信", "Received"],
        "sent": ["上傳流量", "上传流量", "送信", "Sent"],
        "hitRate": ["快取命中率", "缓存命中率", "キャッシュヒット率", "Cache hit rate"],
        "settings": ["設定", "设置", "設定", "Settings"],
        "node": ["節點", "节点", "ノード", "Node"],
        "selection": ["選線方式", "选线方式", "選択方式", "Selection"],
        "auto": ["自動", "自动", "自動", "Automatic"],
        "manual": ["手動", "手动", "手動", "Manual"],
        "chooseNode": ["選擇節點", "选择节点", "ノードを選択", "Choose a node"],
        "cache": ["HTTPS 素材快取", "HTTPS 素材缓存", "HTTPS 素材キャッシュ", "HTTPS asset cache"],
        "save": ["保存", "保存", "保存", "Save"],
        "stopBeforeSettings": ["停止加速後才能修改設定。", "停止加速后才能修改设置。", "設定変更の前に停止してください。", "Stop acceleration before changing settings."],
        "cacheManagement": ["快取管理", "缓存管理", "キャッシュ管理", "Cache management"],
        "exportCertificate": ["產生／匯出 CA 描述檔", "生成／导出 CA 描述文件", "CA プロファイルを作成／書き出す", "Create / export CA profile"],
        "shareCertificate": ["分享公開憑證描述檔", "分享公开证书描述文件", "公開証明書を共有", "Share public certificate profile"],
        "certificateInstructions": ["安裝描述檔後，在「設定 → 一般 → 關於本機 → 憑證信任設定」開啟完整信任，再以 Chrome 驗證。移除 App 不會移除已安裝的 CA。", "安装描述文件后，在「设置 → 通用 → 关于本机 → 证书信任设置」开启完全信任，再用 Chrome 验证。移除 App 不会移除已安装的 CA。", "プロファイルをインストール後、「設定 → 一般 → 情報 → 証明書信頼設定」で完全な信頼を有効にし、Chrome で確認してください。App の削除では CA は削除されません。", "Install the profile, then enable full trust in Settings → General → About → Certificate Trust Settings. Verify in Chrome. Removing the app does not remove the CA."],
        "trusted": ["系統 TLS 信任檢查通過", "系统 TLS 信任检查通过", "システム TLS 信頼を確認済み", "System TLS trust check passed"],
        "untrusted": ["尚未通過系統 TLS 信任檢查", "尚未通过系统 TLS 信任检查", "システム TLS 信頼は未確認", "System TLS trust not confirmed"],
        "clearCache": ["清除快取", "清除缓存", "キャッシュを削除", "Clear cache"],
        "language": ["語言", "语言", "言語", "Language"],
        "theme": ["主題", "主题", "テーマ", "Theme"],
        "light": ["淺色", "浅色", "ライト", "Light"],
        "dark": ["深色", "深色", "ダーク", "Dark"],
        "version": ["版本", "版本", "バージョン", "Version"],
        "done": ["完成", "完成", "完了", "Done"],
        "unbind": ["解除裝置", "解除设备", "デバイス登録を解除", "Unbind device"],
        "unbindConfirmation": ["停止加速並解除此裝置？成功後須重新授權。", "停止加速并解除此设备？成功后须重新授权。", "停止してデバイス登録を解除しますか？再認証が必要になります。", "Stop acceleration and unbind this device? Authorization will be required again."],
        "AUTH_REQUIRED": ["請輸入授權碼。", "请输入授权码。", "認証コードを入力してください。", "Enter an access code."],
        "AUTH_INVALID": ["授權碼無效。", "授权码无效。", "認証コードが無効です。", "Invalid access code."],
        "AUTH_REVOKED": ["授權已撤銷，請重新授權。", "授权已撤销，请重新授权。", "認証が取り消されました。再認証してください。", "Authorization was revoked. Activate again."],
        "AUTH_DEVICE_LIMIT": ["已達授權裝置數量上限。", "已达授权设备数量上限。", "登録可能なデバイス数の上限です。", "The device limit has been reached."],
        "AUTH_DEVICE_BOUND": ["此裝置已綁定其他帳號。", "此设备已绑定其他账号。", "別のアカウントに登録されています。", "This device is bound to another account."],
        "CERTIFICATE_REQUIRED": ["請先安裝並信任本機 CA。", "请先安装并信任本机 CA。", "先に CA をインストールして信頼してください。", "Install and trust the local CA first."],
        "SSH_HOST_KEY_MISMATCH": ["節點身分驗證失敗。", "节点身份验证失败。", "ノードの本人確認に失敗しました。", "Node identity verification failed."],
        "SSH_CONNECTION_FAILED": ["無法連接加速節點。", "无法连接加速节点。", "ノードに接続できません。", "Cannot connect to the acceleration node."],
        "operationFailed": ["操作未完成，請重試。", "操作未完成，请重试。", "操作を完了できませんでした。再試行してください。", "The operation did not complete. Please retry."],
    ]
    static func get(_ key: String, _ language: String) -> String {
        let index = ["zh-TW": 0, "zh-CN": 1, "ja": 2, "en": 3][language] ?? 3
        return values[key]?[index] ?? key
    }
    static func error(_ code: String, _ language: String) -> String {
        let aliases = ["AUTH_NOT_CONFIGURED": "configurationError", "AUTH_UNAVAILABLE": "verificationUnavailable", "STOP_REQUIRED": "stopBeforeSettings", "AUTH_LINE_DENIED": "noNodes"]
        let key = aliases[code] ?? code
        return values[key] == nil ? get("operationFailed", language) + " (" + code + ")" : get(key, language)
    }
}
