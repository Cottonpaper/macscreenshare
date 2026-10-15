// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "macss-host",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "macss-host", targets: ["macss-host"])
    ],
    targets: [
        .executableTarget(
            name: "macss-host",
            path: "Sources/macss-host"
        )
    ]
)
