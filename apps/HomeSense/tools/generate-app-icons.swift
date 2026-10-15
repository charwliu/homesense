#!/usr/bin/env swift
// Package the existing HomeSense brand artwork into Apple asset catalog sizes.
// Run from any directory; no external packages or image-generation service.
import AppKit
import ImageIO
import UniformTypeIdentifiers

let project = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let repository = project.deletingLastPathComponent().deletingLastPathComponent()
let source = repository.appendingPathComponent("docs/assets/homesense-logo-v3.png")
let catalog = project.appendingPathComponent("App/Assets.xcassets")
let output = catalog.appendingPathComponent("AppIcon.appiconset")
let fm = FileManager.default
try fm.createDirectory(at: output, withIntermediateDirectories: true)
guard let imageSource = CGImageSourceCreateWithURL(source as CFURL, nil),
      let logo = CGImageSourceCreateImageAtIndex(imageSource, 0, nil) else {
    fatalError("Cannot load HomeSense brand artwork: \(source.path)")
}
let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!

func render(size: Int, macOS: Bool, name: String) throws {
    // iOS requires an opaque square. macOS uses a transparent outer margin
    // around the platform's rounded-square icon plate.
    let alpha: CGImageAlphaInfo = macOS ? .premultipliedLast : .noneSkipLast
    let context = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
                            bytesPerRow: size * 4, space: colorSpace, bitmapInfo: alpha.rawValue)!
    context.scaleBy(x: CGFloat(size) / 1024, y: CGFloat(size) / 1024)
    context.setFillColor(CGColor(colorSpace: colorSpace, components: [0.98, 0.98, 0.97, 1])!)
    if macOS {
        context.addPath(CGPath(roundedRect: CGRect(x: 64, y: 64, width: 896, height: 896),
                               cornerWidth: 196, cornerHeight: 196, transform: nil))
        context.fillPath()
    } else {
        context.fill(CGRect(x: 0, y: 0, width: 1024, height: 1024))
    }
    context.interpolationQuality = .high
    let inset: CGFloat = macOS ? 112 : 72
    context.draw(logo, in: CGRect(x: inset, y: inset, width: 1024 - inset * 2, height: 1024 - inset * 2))
    let image = context.makeImage()!
    let destination = CGImageDestinationCreateWithURL(output.appendingPathComponent(name) as CFURL,
                                                      UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { fatalError("Cannot write \(name)") }
}

var entries: [[String: String]] = []
try render(size: 1024, macOS: false, name: "ios-1024.png")
entries.append(["filename": "ios-1024.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"])
for pointSize in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = "mac-\(pointSize)@\(scale)x.png"
        try render(size: pointSize * scale, macOS: true, name: name)
        entries.append(["filename": name, "idiom": "mac", "size": "\(pointSize)x\(pointSize)", "scale": "\(scale)x"])
    }
}
let info: [String: Any] = ["author": "xcode", "version": 1]
try JSONSerialization.data(withJSONObject: ["images": entries, "info": info], options: [.prettyPrinted, .sortedKeys])
    .write(to: output.appendingPathComponent("Contents.json"))
try JSONSerialization.data(withJSONObject: ["info": info], options: [.prettyPrinted, .sortedKeys])
    .write(to: catalog.appendingPathComponent("Contents.json"))
print("Generated AppIcon: opaque iOS 1024 + 10 macOS sizes using homesense-logo-v3.png")
