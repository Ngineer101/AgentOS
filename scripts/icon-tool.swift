// PNG helpers for scripts/make-icons.sh (CoreGraphics, no dependencies).
//
//   swift scripts/icon-tool.swift flatten in.png out.png
//       redraw without an alpha channel (App Store icons must be opaque)
//   swift scripts/icon-tool.swift matte on-black.png on-white.png out.png
//       recover transparency from two renders of the same art over black and over white:
//       alpha = 1 - (white - black), colour = black / alpha
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

func load(_ path: String) -> CGImage {
    guard let s = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let i = CGImageSourceCreateImageAtIndex(s, 0, nil) else { fatalError("cannot read \(path)") }
    return i
}

func rgba(_ img: CGImage) -> [UInt8] {
    var px = [UInt8](repeating: 0, count: img.width * img.height * 4)
    let ctx = CGContext(data: &px, width: img.width, height: img.height, bitsPerComponent: 8,
                        bytesPerRow: img.width * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: img.width, height: img.height))
    return px
}

func save(_ px: [UInt8], _ w: Int, _ h: Int, alpha: Bool, _ path: String) {
    var data = px
    let info = alpha ? CGImageAlphaInfo.premultipliedLast : CGImageAlphaInfo.noneSkipLast
    let ctx = CGContext(data: &data, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                        space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: info.rawValue)!
    let dst = CGImageDestinationCreateWithURL(URL(fileURLWithPath: path) as CFURL,
                                              UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dst, ctx.makeImage()!, nil)
    guard CGImageDestinationFinalize(dst) else { fatalError("cannot write \(path)") }
}

let a = CommandLine.arguments
switch (a.count > 1 ? a[1] : "", a.count) {
case ("flatten", 4):
    let img = load(a[2])
    var px = rgba(img)
    for i in stride(from: 0, to: px.count, by: 4) { px[i + 3] = 255 } // over black: premultiplied = straight
    save(px, img.width, img.height, alpha: false, a[3])
case ("matte", 5):
    let b = load(a[2]), w = load(a[3])
    guard b.width == w.width, b.height == w.height else { fatalError("sizes differ") }
    let pb = rgba(b), pw = rgba(w)
    var out = [UInt8](repeating: 0, count: pb.count)
    for i in stride(from: 0, to: pb.count, by: 4) {
        // Mean over the channels; each channel gives the same alpha up to rounding.
        var diff = 0
        for c in 0..<3 { diff += Int(pw[i + c]) - Int(pb[i + c]) }
        let alpha = max(0, min(255, 255 - diff / 3))
        // Over black, the premultiplied colour is the black render itself.
        for c in 0..<3 { out[i + c] = UInt8(min(Int(pb[i + c]), alpha)) }
        out[i + 3] = UInt8(alpha)
    }
    save(out, b.width, b.height, alpha: true, a[4])
default:
    fatalError("usage: icon-tool.swift flatten in.png out.png | matte black.png white.png out.png")
}
