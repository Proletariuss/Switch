import SwiftUI

/// Transparent Telegram-style white plane used for SMS send and forwarding actions.
/// The square canvas and fitted plane keep the mark proportional at every size.
struct TelegramPlaneIcon: View {
    var size: CGFloat = 20

    var body: some View {
        TelegramPaperPlaneShape()
            .fill(.white, style: FillStyle(eoFill: true))
            .padding(size * 0.04)
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

private struct TelegramPaperPlaneShape: Shape {
    func path(in rect: CGRect) -> Path {
        func point(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            CGPoint(x: rect.minX + rect.width * x, y: rect.minY + rect.height * y)
        }

        var plane = Path()
        plane.move(to: point(0.08, 0.47))
        plane.addCurve(to: point(0.90, 0.12), control1: point(0.34, 0.36), control2: point(0.77, 0.16))
        plane.addCurve(to: point(0.94, 0.18), control1: point(0.95, 0.08), control2: point(0.96, 0.11))
        plane.addLine(to: point(0.74, 0.84))
        plane.addCurve(to: point(0.65, 0.87), control1: point(0.72, 0.92), control2: point(0.69, 0.91))
        plane.addLine(to: point(0.45, 0.69))
        plane.addLine(to: point(0.33, 0.82))
        plane.addCurve(to: point(0.29, 0.79), control1: point(0.31, 0.85), control2: point(0.29, 0.84))
        plane.addLine(to: point(0.32, 0.60))
        plane.addLine(to: point(0.09, 0.52))
        plane.addCurve(to: point(0.08, 0.47), control1: point(0.03, 0.50), control2: point(0.04, 0.48))
        plane.closeSubpath()

        // Transparent negative space suggests the folded wing and reveals the control's own background.
        var fold = Path()
        fold.move(to: point(0.32, 0.60))
        fold.addLine(to: point(0.77, 0.29))
        fold.addLine(to: point(0.49, 0.66))
        fold.addLine(to: point(0.43, 0.77))
        fold.addLine(to: point(0.43, 0.66))
        fold.closeSubpath()
        plane.addPath(fold)
        return plane
    }
}
