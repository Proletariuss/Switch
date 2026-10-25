import Foundation

@main
private enum WorkspaceViewportTests {
    static func main() {
        let regular = WorkspaceViewport(width: 960, height: 720)
        precondition(!regular.isCompactHeight && !regular.isVeryShort)
        precondition(regular.pageInset == 24 && regular.cardPadding == 20)
        precondition(regular.contentWidth == 912)
        precondition(regular.columnCount(minimumWidth: 318) == 2)

        let compact = WorkspaceViewport(width: 960, height: 719)
        precondition(compact.isCompactHeight && !compact.isVeryShort)
        precondition(compact.pageInset == 18 && compact.sectionSpacing == 14)
        precondition(compact.cardPadding == 16)
        precondition(WorkspaceViewport(height: 580).isVeryShort == false)

        let short = WorkspaceViewport(width: 688, height: 550)
        precondition(short.isVeryShort)
        precondition(short.pageInset == 12 && short.sectionSpacing == 10)
        precondition(short.cardPadding == 14)
        precondition(short.columnCount(minimumWidth: 312) == 2)

        let exact = WorkspaceViewport(width: 678, height: 650)
        precondition(exact.columnCount(minimumWidth: 314) == 2)
        precondition(WorkspaceViewport(width: 677, height: 650).columnCount(minimumWidth: 314) == 1)
        let wide = WorkspaceViewport(width: 1600, height: 1000)
        precondition(wide.columnCount(minimumWidth: 318) == 3)
        precondition(wide.columnCount(minimumWidth: 318, maximum: 4) == 4)
        precondition(wide.columnCount(minimumWidth: 318, maximum: 0) == 1)
        precondition(wide.columnCount(minimumWidth: 0) == 1)
        precondition(wide.columnCount(minimumWidth: .nan) == 1)
        precondition(WorkspaceViewport(width: .infinity).columnCount(minimumWidth: 300) == 1)
        precondition(WorkspaceViewport(width: .greatestFiniteMagnitude).columnCount(minimumWidth: 300) == 3)
        precondition(WorkspaceViewport(width: -5).columnCount(minimumWidth: 300) == 1)
        precondition(WorkspaceViewport(width: 0, height: 0).columnCount(minimumWidth: 300) == 1)
        print("Workspace viewport: height density, column breakpoints and invalid-size fallbacks passed")
    }
}
