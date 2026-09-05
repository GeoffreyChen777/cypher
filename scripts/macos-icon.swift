#!/usr/bin/env swift
// Normalize only the shared artwork's alpha channel for macOS IconServices.
// A nearly opaque tile is not an opaque tile: macOS 26 puts the former on a
// second backplate and shrinks it. Keep RGB, geometry, and edge antialiasing.
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

enum IconError: Error, CustomStringConvertible {
    case invalid(String)

    var description: String {
        switch self {
        case .invalid(let message): return message
        }
    }
}

struct IconRaster {
    let width: Int
    let height: Int
    let colorSpace: CGColorSpace
    var rgba: [UInt8]

    init(path: String) throws {
        let url = URL(fileURLWithPath: path)
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              CGImageSourceGetType(source) as String? == UTType.png.identifier,
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
              image.width == 1024, image.height == 1024,
              image.bitsPerComponent == 8, image.bitsPerPixel == 32,
              image.alphaInfo == .last,
              image.bitmapInfo.intersection(.byteOrderMask).rawValue == 0,
              let space = image.colorSpace, space.model == .rgb,
              let data = image.dataProvider?.data else {
            throw IconError.invalid("Expected a 1024×1024 PNG with straight 8-bit RGBA pixels: \(path)")
        }
        width = image.width
        height = image.height
        colorSpace = space
        let bytes = [UInt8](data as Data)
        guard image.bytesPerRow >= width * 4, bytes.count >= image.bytesPerRow * height else {
            throw IconError.invalid("Invalid PNG row storage: \(path)")
        }
        // Read straight RGBA, not through a premultiplied CGContext: the latter
        // would round RGB values and unnecessarily alter the existing artwork.
        rgba = []
        rgba.reserveCapacity(width * height * 4)
        for row in 0..<height {
            let start = row * image.bytesPerRow
            rgba.append(contentsOf: bytes[start..<(start + width * 4)])
        }
    }

    func write(to path: String) throws {
        guard let provider = CGDataProvider(data: Data(rgba) as CFData),
              let image = CGImage(
                width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
                bytesPerRow: width * 4, space: colorSpace,
                bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent
              ),
              let encoded = CFDataCreateMutable(nil, 0),
              let destination = CGImageDestinationCreateWithData(
                encoded, UTType.png.identifier as CFString, 1, nil
              ) else {
            throw IconError.invalid("Cannot create macOS icon PNG.")
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw IconError.invalid("Cannot encode macOS icon PNG.")
        }
        try (encoded as Data).write(to: URL(fileURLWithPath: path), options: .atomic)
    }
}

func normalizedAlpha(_ source: [UInt8]) -> [UInt8] {
    precondition(source.count.isMultiple(of: 4))
    var result = source
    for pixel in stride(from: 0, to: result.count, by: 4) {
        switch result[pixel + 3] {
        case 0..<16:
            // Remove barely visible background specks, including hidden RGB.
            result[pixel] = 0
            result[pixel + 1] = 0
            result[pixel + 2] = 0
            result[pixel + 3] = 0
        case 240...255:
            // The icon face must be genuinely opaque for native normalization.
            result[pixel + 3] = 255
        default:
            // Retain the rounded outline's real antialiasing, not a hard mask.
            break
        }
    }
    return result
}

func check(source: String, output: String) throws {
    let artwork = try IconRaster(path: source)
    let icon = try IconRaster(path: output)
    guard icon.rgba == normalizedAlpha(artwork.rgba),
          CFEqual(artwork.colorSpace, icon.colorSpace) else {
        throw IconError.invalid(
            "macOS icon is stale or has unsafe alpha. Regenerate with:\n"
            + "  xcrun swift scripts/macos-icon.swift generate \(source) \(output)"
        )
    }
    // The committed tile should have opaque content, transparent padding, and
    // no near-opaque face pixels. Checking only the canvas size missed this bug.
    let alpha = stride(from: 3, to: icon.rgba.count, by: 4).map { icon.rgba[$0] }
    guard alpha.contains(255), alpha.contains(0),
          !alpha.contains(where: { (1..<16).contains($0) || (240..<255).contains($0) }) else {
        throw IconError.invalid("macOS icon lacks a clean opaque face or transparent padding.")
    }
    print("macOS icon checked: 1024×1024, normalized alpha, original visible RGB and layout preserved.")
}

func selfTest() throws {
    let source: [UInt8] = (0...255).flatMap { [17, 83, 191, UInt8($0)] }
    let result = normalizedAlpha(source)
    guard result.count == source.count, normalizedAlpha(result) == result else {
        throw IconError.invalid("Alpha normalization must preserve dimensions and be idempotent.")
    }
    for alpha in 0...255 {
        let i = alpha * 4
        let actual = Array(result[i..<(i + 4)])
        let expected: [UInt8]
        if alpha < 16 {
            expected = [0, 0, 0, 0]
        } else if alpha >= 240 {
            expected = [17, 83, 191, 255]
        } else {
            expected = [17, 83, 191, UInt8(alpha)]
        }
        guard actual == expected else {
            throw IconError.invalid("Unexpected RGB/alpha change at alpha \(alpha).")
        }
    }
    print("Alpha tests passed: all 256 alpha levels, RGB preservation, antialiased edges, and idempotence.")
}

do {
    let args = Array(CommandLine.arguments.dropFirst())
    if args == ["test"] {
        try selfTest()
    } else if args.count == 3 && args[0] == "generate" {
        let input = URL(fileURLWithPath: args[1]).resolvingSymlinksInPath().standardizedFileURL
        let output = URL(fileURLWithPath: args[2]).resolvingSymlinksInPath().standardizedFileURL
        guard input != output else {
            throw IconError.invalid("Use a separate macOS output; do not overwrite the shared artwork.")
        }
        var icon = try IconRaster(path: args[1])
        icon.rgba = normalizedAlpha(icon.rgba)
        try icon.write(to: args[2])
        try check(source: args[1], output: args[2])
    } else if args.count == 3 && args[0] == "check" {
        try check(source: args[1], output: args[2])
    } else {
        throw IconError.invalid(
            "Usage: xcrun swift scripts/macos-icon.swift "
            + "generate|check <shared.png> <macos.png>\n"
            + "       xcrun swift scripts/macos-icon.swift test"
        )
    }
} catch {
    fputs("macos-icon: \(error)\n", stderr)
    exit(1)
}
