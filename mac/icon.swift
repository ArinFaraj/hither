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
    let drop = NSBezierPath()
    drop.move(to: p(54, 30))
    drop.curve(to: p(38, 60), controlPoint1: p(54, 30), controlPoint2: p(38, 48))
    drop.appendArc(withCenter: p(54, 60), radius: 16 * u, startAngle: 180, endAngle: 360, clockwise: false)
    drop.curve(to: p(54, 30), controlPoint1: p(70, 48), controlPoint2: p(54, 30))
    drop.close()
    NSColor.white.setFill()
    drop.fill()
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for size in [16, 32, 128, 256, 512] {
    try! render(size).write(to: URL(fileURLWithPath: "\(out)/icon_\(size)x\(size).png"))
    try! render(size * 2).write(to: URL(fileURLWithPath: "\(out)/icon_\(size)x\(size)@2x.png"))
}
