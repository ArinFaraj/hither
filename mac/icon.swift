import AppKit

let out = CommandLine.arguments[1]
try? FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true)

func render(_ px: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8, samplesPerPixel: 4,
                               hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = CGFloat(px)
    let inset = s * 0.1
    let tile = NSRect(x: inset, y: inset, width: s - 2 * inset, height: s - 2 * inset)
    let bg = NSBezierPath(roundedRect: tile, xRadius: tile.width * 0.225, yRadius: tile.width * 0.225)
    NSGradient(starting: NSColor(srgbRed: 0.33, green: 0.62, blue: 1.0, alpha: 1),
               ending: NSColor(srgbRed: 0.10, green: 0.36, blue: 0.90, alpha: 1))!.draw(in: bg, angle: -90)
    let u = tile.width / 72
    func p(_ x: CGFloat, _ y: CGFloat) -> NSPoint { NSPoint(x: tile.minX + (x - 18) * u, y: tile.maxY - (y - 18) * u) }
    NSColor.white.set()
    let arrows = NSBezierPath()
    for (a, b, c) in [((44, 32), (54, 42), (64, 32)), ((44, 76), (54, 66), (64, 76)),
                      ((32, 44), (42, 54), (32, 64)), ((76, 44), (66, 54), (76, 64))] as [((CGFloat, CGFloat), (CGFloat, CGFloat), (CGFloat, CGFloat))] {
        arrows.move(to: p(a.0, a.1))
        arrows.line(to: p(b.0, b.1))
        arrows.line(to: p(c.0, c.1))
    }
    arrows.lineWidth = 6 * u
    arrows.lineCapStyle = .round
    arrows.lineJoinStyle = .round
    arrows.stroke()
    let c = p(54, 54)
    NSBezierPath(ovalIn: NSRect(x: c.x - 7 * u, y: c.y - 7 * u, width: 14 * u, height: 14 * u)).fill()
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for size in [16, 32, 128, 256, 512] {
    try! render(size).write(to: URL(fileURLWithPath: "\(out)/icon_\(size)x\(size).png"))
    try! render(size * 2).write(to: URL(fileURLWithPath: "\(out)/icon_\(size)x\(size)@2x.png"))
}
