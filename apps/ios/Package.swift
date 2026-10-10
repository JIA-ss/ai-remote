// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "AIRemotePhone",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "AIRemotePhone", targets: ["AIRemotePhone"])],
    targets: [
        .binaryTarget(name: "AIRemoteCoreFFI", path: "Generated/AIRemoteCoreFFI.xcframework"),
        .target(name: "AIRemoteCore", dependencies: ["AIRemoteCoreFFI"], path: "Generated", exclude: ["AIRemoteCoreFFI.xcframework", "Headers", "AIRemoteCoreFFI.h", "AIRemoteCoreFFI.modulemap", "module.modulemap"], sources: ["AIRemoteCore.swift"], linkerSettings: [.linkedFramework("SystemConfiguration"), .linkedFramework("Security"), .linkedFramework("CoreFoundation"), .linkedFramework("Network"), .linkedLibrary("iconv")]),
        .target(name: "AIRemotePhone", dependencies: ["AIRemoteCore"], path: "Sources"),
    ],
    swiftLanguageModes: [.v5]
)
