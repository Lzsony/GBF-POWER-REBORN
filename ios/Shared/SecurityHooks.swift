import Foundation
import Security

enum SharedEnvironment {
    static func value(_ key: String) throws -> String {
        guard let value = Bundle.main.object(forInfoDictionaryKey: key) as? String,
              !value.isEmpty, !value.contains("$(") else { throw CoreFailure("CONFIG_READ_FAILED") }
        return value
    }

    static func root() throws -> URL {
        let group = try value("GPRAppGroup")
        guard let url = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group) else {
            throw CoreFailure("CONFIG_READ_FAILED")
        }
        let root = url.appendingPathComponent("GPR", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true,
                                               attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
        var excluded = root
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try excluded.setResourceValues(values)
        return root
    }
}

private func secretQuery(_ name: UnsafePointer<CChar>?) -> [String: Any]? {
    guard let name, let group = try? SharedEnvironment.value("GPRKeychainGroup") else { return nil }
    return [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "cc.lzsony.gpr.ios",
            kSecAttrAccount as String: String(cString: name),
            kSecAttrAccessGroup as String: group,
            kSecAttrSynchronizable as String: false]
}

@_cdecl("gpr_ios_secret_read")
func nativeSecretRead(_ name: UnsafePointer<CChar>?, _ status: UnsafeMutablePointer<Int32>?) -> UnsafeMutablePointer<CChar>? {
    guard var query = secretQuery(name) else { status?.pointee = -1; return nil }
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var result: CFTypeRef?
    let code = SecItemCopyMatching(query as CFDictionary, &result)
    guard code == errSecSuccess, let data = result as? Data, let text = String(data: data, encoding: .utf8) else {
        status?.pointee = code == errSecItemNotFound ? 1 : -1
        return nil
    }
    status?.pointee = 0
    return strdup(text)
}

@_cdecl("gpr_ios_secret_write")
func nativeSecretWrite(_ name: UnsafePointer<CChar>?, _ value: UnsafePointer<CChar>?) -> Int32 {
    guard let query = secretQuery(name), let value else { return -1 }
    let attributes: [String: Any] = [kSecValueData as String: Data(String(cString: value).utf8),
                                    kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly]
    let updated = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
    if updated == errSecItemNotFound {
        var insert = query
        attributes.forEach { insert[$0.key] = $0.value }
        return SecItemAdd(insert as CFDictionary, nil) == errSecSuccess ? 0 : -1
    }
    return updated == errSecSuccess ? 0 : -1
}

@_cdecl("gpr_ios_secret_delete")
func nativeSecretDelete(_ name: UnsafePointer<CChar>?) -> Int32 {
    guard let query = secretQuery(name) else { return -1 }
    switch SecItemDelete(query as CFDictionary) { case errSecSuccess: return 0; case errSecItemNotFound: return 1; default: return -1 }
}

@_cdecl("gpr_ios_secret_free")
func nativeSecretFree(_ value: UnsafeMutablePointer<CChar>?) { free(value) }

@_cdecl("gpr_ios_certificate_trusted")
func nativeCertificateTrusted(_ pem: UnsafePointer<CChar>?, _ leaf: UnsafePointer<UInt8>?, _ length: Int) -> Bool {
    guard let pem, let leaf, length > 0 else { return false }
    let base64 = String(cString: pem).components(separatedBy: .newlines).filter { !$0.hasPrefix("-----") }.joined()
    guard let rootData = Data(base64Encoded: base64),
          let root = SecCertificateCreateWithData(nil, rootData as CFData),
          let probe = SecCertificateCreateWithData(nil, Data(bytes: leaf, count: length) as CFData) else { return false }
    var trust: SecTrust?
    let policy = SecPolicyCreateSSL(true, "gpr-cache-check.invalid" as CFString)
    guard SecTrustCreateWithCertificates([probe, root] as CFArray, policy, &trust) == errSecSuccess,
          let trust else { return false }
    // Do not add the app's CA as an explicit trust anchor: require system/user trust.
    SecTrustSetNetworkFetchAllowed(trust, false)
    return SecTrustEvaluateWithError(trust, nil)
}
