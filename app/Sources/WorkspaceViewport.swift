import Foundation

/// Actual detail-area dimensions, not screen size. Presentation density can
/// react to resizing without changing stored preferences or business state.
struct WorkspaceViewport: Equatable {
    var width: CGFloat = 960
    var height: CGFloat = 720

    var isCompactHeight: Bool { height < 720 }
    var isVeryShort: Bool { height < 580 }
    var pageInset: CGFloat { isVeryShort ? 12 : (isCompactHeight ? 18 : 24) }
    var sectionSpacing: CGFloat { isVeryShort ? 10 : (isCompactHeight ? 14 : 18) }
    var cardPadding: CGFloat { isVeryShort ? 14 : (isCompactHeight ? 16 : 20) }
    var contentWidth: CGFloat { max(0, width - pageInset * 2) }

    func columnCount(minimumWidth: CGFloat, maximum: Int = 3) -> Int {
        guard minimumWidth.isFinite, minimumWidth > 0,
              contentWidth.isFinite else { return 1 }
        let limit = max(1, maximum)
        let fitted = floor((contentWidth + sectionSpacing) / (minimumWidth + sectionSpacing))
        // Clamp before integer conversion, including unusually large finite
        // layout proposals; resizing must never trap on a numeric conversion.
        if fitted >= CGFloat(limit) { return limit }
        return max(1, Int(fitted))
    }
}
