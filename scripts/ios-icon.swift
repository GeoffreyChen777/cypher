#!/usr/bin/env swift
// iOS owns the icon's corner mask. Crop the shared desktop-style tile's
// transparent margin, preserve its artwork/aspect ratio, and flatten to RGB.
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

enum IconError: Error {
    case invalid(String)
}

func load(_ path: String) throws -> CGImage {
    guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          CGImageSourceGetType(source) as String? == UTType.png.identifier,
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
          image.width == 1024, image.height == 1024,
          image.bitsPerComponent == 8, image.colorSpace?.model == .rgb else {
        throw IconError.invalid("Expected a 1024×1024 RGB PNG: \(path)")
    }
    return image
}

func faceSquare(width: Int, height: Int, alpha: (Int, Int) -> UInt8) throws -> CGRect {
    var left = width, top = height, right = -1, bottom = -1
    for y in 0..<height {
        for x in 0..<width where alpha(x, y) >= 240 {
            left = min(left, x); top = min(top, y)
            right = max(right, x); bottom = max(bottom, y)
        }
    }
    let w = right - left + 1, h = bottom - top + 1
    guard w >= width / 2, h >= height / 2,
          abs(w - h) <= max(2, max(w, h) / 20) else {
        throw IconError.invalid("No near-square opaque icon face found.")
    }
    // Inscribed square: no aspect distortion and no residual outer strip.
    let side = min(w, h)
    return CGRect(x: left + (w - side) / 2, y: top + (h - side) / 2,
                  width: side, height: side)
}

func generate(_ source: CGImage) throws -> (CGImage, CGRect) {
    guard source.bitsPerPixel == 32, source.alphaInfo == .last,
          let bytes = source.dataProvider?.data,
          let space = source.colorSpace else {
        throw IconError.invalid("Shared artwork must use straight RGBA pixels.")
    }
    let rgba = [UInt8](bytes as Data)
    let crop = try faceSquare(width: source.width, height: source.height) { x, y in
        rgba[y * source.bytesPerRow + x * 4 + 3]
    }
    guard let face = source.cropping(to: crop),
          let context = CGContext(data: nil, width: 1024, height: 1024,
              bitsPerComponent: 8, bytesPerRow: 1024 * 4, space: space,
              bitmapInfo: CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.noneSkipLast.rawValue) else {
        throw IconError.invalid("Cannot allocate the iOS icon raster.")
    }
    // Use the artwork's own quiet background, not a new logo/backplate.
    // Filling the old rounded corners makes the source truly full-bleed;
    // iOS applies the final system mask exactly once.
    let sampleX = Int(crop.minX + crop.width * 0.1), sampleY = Int(crop.midY)
    let i = sampleY * source.bytesPerRow + sampleX * 4
    let components = rgba[i..<(i + 3)].map { CGFloat($0) / 255 } + [CGFloat(1)]
    guard let background = CGColor(colorSpace: space, components: components) else {
        throw IconError.invalid("Cannot read the source background color.")
    }
    context.setFillColor(background)
    let canvas = CGRect(x: 0, y: 0, width: 1024, height: 1024)
    context.fill(canvas)
    context.interpolationQuality = .high
    context.draw(face, in: canvas)
    guard let image = context.makeImage() else { throw IconError.invalid("Cannot render icon.") }
    return (image, crop)
}

func rgb(_ image: CGImage) throws -> [UInt8] {
    guard let data = image.dataProvider?.data, [24, 32].contains(image.bitsPerPixel) else {
        throw IconError.invalid("Unsupported RGB pixel layout.")
    }
    let bytes = [UInt8](data as Data), step = image.bitsPerPixel / 8
    var result: [UInt8] = []
    result.reserveCapacity(image.width * image.height * 3)
    for y in 0..<image.height {
        for x in 0..<image.width {
            let i = y * image.bytesPerRow + x * step
            result.append(contentsOf: bytes[i..<(i + 3)])
        }
    }
    return result
}

func write(_ image: CGImage, to path: String) throws {
    let data = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil) else {
        throw IconError.invalid("Cannot encode PNG.")
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { throw IconError.invalid("Cannot finalize PNG.") }
    try (data as Data).write(to: URL(fileURLWithPath: path), options: .atomic)
}

do {
    let args = Array(CommandLine.arguments.dropFirst())
    if args == ["test"] {
        let crop = try faceSquare(width: 20, height: 20) { x, y in
            (3...16).contains(x) && (2...17).contains(y) ? 252 : 0
        }
        guard crop == CGRect(x: 3, y: 3, width: 14, height: 14) else {
            throw IconError.invalid("Crop must remove padding without stretching.")
        }
        do {
            _ = try faceSquare(width: 20, height: 20) { _, _ in 0 }
            throw IconError.invalid("Empty artwork was accepted.")
        } catch IconError.invalid(let message) where message == "No near-square opaque icon face found." {}
        print("iOS icon geometry tests passed.")
    } else if args.count == 3, ["generate", "check"].contains(args[0]) {
        let sourceURL = URL(fileURLWithPath: args[1]).resolvingSymlinksInPath().standardizedFileURL
        let outputURL = URL(fileURLWithPath: args[2]).resolvingSymlinksInPath().standardizedFileURL
        guard sourceURL != outputURL else { throw IconError.invalid("Do not overwrite shared artwork.") }
        let (expected, crop) = try generate(load(args[1]))
        if args[0] == "generate" { try write(expected, to: args[2]) }
        let output = try load(args[2])
        guard [.none, .noneSkipLast].contains(output.alphaInfo),
              try rgb(output) == rgb(expected) else {
            throw IconError.invalid("iOS icon is stale or still has transparency. Regenerate it.")
        }
        print("iOS icon checked: 1024×1024 opaque RGB, full-bleed crop \(crop).")
    } else {
        throw IconError.invalid("Usage: swift scripts/ios-icon.swift test | generate|check SOURCE OUTPUT")
    }
} catch {
    fputs("\(error)\n", stderr)
    exit(1)
}
