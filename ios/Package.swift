// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "GPRAdmission", products: [.library(name: "GPRAdmission", targets: ["GPRAdmission"])], targets: [
    .target(name: "GPRAdmission", path: "Shared", exclude: ["GPRCore.h", "SecurityHooks.swift", "CoreClient.swift"], sources: ["AuthorizationGate.swift"]),
    .testTarget(name: "GPRAdmissionTests", dependencies: ["GPRAdmission"], path: "Tests")
])
