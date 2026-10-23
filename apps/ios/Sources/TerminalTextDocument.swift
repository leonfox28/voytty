import UIKit

/// UTF-16 positions for UIKit, with geometry taken from semantic cells, not font shaping.
@MainActor
struct TerminalTextDocument {
    struct Cell {
        let range: NSRange
        let row: Int64
        let column: Int
        let width: Int
    }
    let text: NSString
    let cells: [Cell]
    let rowCount: Int
    let firstRow: Int64
    let rows: [NativeRow]
    private let rowStarts: [Int]
    enum MergeError: Error { case changed, gap, limit }
    static let maximumCells = 65_536
    static let maximumTextBytes = 512 * 1024

    init(rows: [NativeRow] = [], firstRow: Int64 = 0) {
        var text = ""
        var cells: [Cell] = []
        var offset = 0
        var starts: [Int] = []
        for (rowIndex, row) in rows.enumerated() {
            starts.append(offset)
            for (column, cell) in row.cells.enumerated() where cell.width > 0 {
                let value = cell.text.isEmpty ? " " : cell.text
                let length = value.utf16.count
                cells.append(Cell(range: NSRange(location: offset, length: length), row: firstRow + Int64(rowIndex),
                                  column: column, width: Int(cell.width)))
                text += value
                offset += length
            }
            if !row.wrapped && rowIndex + 1 < rows.count { text += "\n"; offset += 1 }
        }
        self.text = text as NSString
        self.cells = cells
        rowCount = rows.count
        self.firstRow = firstRow
        self.rows = rows
        rowStarts = starts
    }

    /// Only retain a contiguous selected span plus the currently supplied presentation window.
    func merging(_ incoming: [NativeRow], first: Int64, keeping selected: ClosedRange<Int64>) throws -> Self {
        let low = min(first, selected.lowerBound)
        let high = max(first + Int64(incoming.count) - 1, selected.upperBound)
        guard high >= low, high - low < 4096 else { throw MergeError.limit }
        var merged: [NativeRow] = []
        var cellCount = 0
        var bytes = 0
        for logical in low...high {
            let oldIndex = Int(logical - firstRow), newIndex = Int(logical - first)
            let old = rows.indices.contains(oldIndex) ? rows[oldIndex] : nil
            let new = incoming.indices.contains(newIndex) ? incoming[newIndex] : nil
            if let old, let new, selected.contains(logical), old != new { throw MergeError.changed }
            guard let row = new ?? old else { throw MergeError.gap }
            cellCount += row.cells.count
            bytes += row.cells.reduce(1) { $0 + max(1, $1.text.utf8.count) }
            guard cellCount <= Self.maximumCells, bytes <= Self.maximumTextBytes else { throw MergeError.limit }
            merged.append(row)
        }
        return Self(rows: merged, firstRow: low)
    }

    func position(at offset: Int) -> TerminalTextPosition {
        let value = max(0, min(text.length, offset))
        let index = rowStarts.lastIndex(where: { $0 <= value }) ?? 0
        return TerminalTextPosition(row: firstRow + Int64(index), column: value - (rowStarts.indices.contains(index) ? rowStarts[index] : 0))
    }

    func offset(of position: TerminalTextPosition) -> Int? {
        let index = Int(position.row - firstRow)
        guard rowStarts.indices.contains(index) else { return nil }
        let end = index + 1 < rowStarts.count ? rowStarts[index + 1] : text.length
        let value = rowStarts[index] + position.column
        return value >= rowStarts[index] && value <= end ? value : nil
    }

    func range(_ range: TerminalTextRange) -> NSRange? {
        guard let first = offset(of: range.lower), let last = offset(of: range.upper) else { return nil }
        return NSRange(location: min(first, last), length: abs(last - first))
    }

    func textRange(_ value: NSRange) -> TerminalTextRange {
        TerminalTextRange(position(at: value.location), position(at: NSMaxRange(value)))
    }

    func normalized(_ range: NSRange) -> NSRange {
        let lower = max(0, min(text.length, range.location))
        let upper = max(lower, min(text.length, range.location + range.length))
        guard upper > lower else { return NSRange(location: lower, length: 0) }
        let selected = cells.filter { NSIntersectionRange($0.range, NSRange(location: lower, length: upper - lower)).length > 0 }
        guard let first = selected.first, let last = selected.last else { return NSRange(location: lower, length: 0) }
        return NSRange(location: first.range.location, length: NSMaxRange(last.range) - first.range.location)
    }

    func endpoints(_ range: NSRange) -> (Cell, Cell)? {
        let selected = cells.filter { NSIntersectionRange($0.range, range).length > 0 }
        guard let first = selected.first, let last = selected.last else { return nil }
        return (first, last)
    }

    func cell(at point: CGPoint, size: CGSize, visibleFirstRow: Int64? = nil) -> Cell? {
        let row = max(firstRow, min(firstRow + Int64(rowCount) - 1, (visibleFirstRow ?? firstRow) + Int64(floor(point.y / size.height))))
        let line = cells.filter { $0.row == row }
        return line.first { point.x < CGFloat($0.column + $0.width) * size.width } ?? line.last
    }

    func closestOffset(to point: CGPoint, size: CGSize, visibleFirstRow: Int64? = nil) -> Int {
        guard let cell = cell(at: point, size: size, visibleFirstRow: visibleFirstRow) else { return 0 }
        let midpoint = (CGFloat(cell.column) + CGFloat(cell.width) / 2) * size.width
        return point.x < midpoint ? cell.range.location : NSMaxRange(cell.range)
    }

    func caret(at offset: Int, size: CGSize, visibleFirstRow: Int64? = nil) -> CGRect {
        let cell = cells.first { NSMaxRange($0.range) > offset } ?? cells.last
        guard let cell else { return CGRect(origin: .zero, size: CGSize(width: 2, height: size.height)) }
        let column = cell.column + (offset >= NSMaxRange(cell.range) ? cell.width : 0)
        return CGRect(x: CGFloat(column) * size.width, y: CGFloat(cell.row - (visibleFirstRow ?? firstRow)) * size.height, width: 2, height: size.height)
    }

    func rectangles(for range: NSRange, size: CGSize, visibleFirstRow: Int64? = nil) -> [CGRect] {
        var result: [CGRect] = []
        for cell in cells where NSIntersectionRange(cell.range, range).length > 0 {
            let rect = CGRect(x: CGFloat(cell.column) * size.width, y: CGFloat(cell.row - (visibleFirstRow ?? firstRow)) * size.height,
                              width: CGFloat(cell.width) * size.width, height: size.height)
            if result.last?.minY == rect.minY { result[result.count - 1] = result.last!.union(rect) }
            else { result.append(rect) }
        }
        return result
    }
}

final class TerminalTextPosition: UITextPosition {
    // Logical row/UTF-16 column survive prepending history while UIKit is dragging a handle.
    let row: Int64
    let column: Int
    init(row: Int64, column: Int) { self.row = row; self.column = column; super.init() }
    func matches(_ other: TerminalTextPosition) -> Bool { row == other.row && column == other.column }
}

final class TerminalTextRange: UITextRange {
    let lower: TerminalTextPosition
    let upper: TerminalTextPosition
    init(_ lower: TerminalTextPosition, _ upper: TerminalTextPosition) { self.lower = lower; self.upper = upper; super.init() }
    override var start: UITextPosition { lower }
    override var end: UITextPosition { upper }
    override var isEmpty: Bool { lower.matches(upper) }
}

final class TerminalSelectionRect: UITextSelectionRect {
    private let box: CGRect
    private let first: Bool
    private let last: Bool
    init(_ box: CGRect, first: Bool, last: Bool) {
        self.box = box; self.first = first; self.last = last
        super.init()
    }
    override var rect: CGRect { box }
    override var containsStart: Bool { first }
    override var containsEnd: Bool { last }
    override var writingDirection: NSWritingDirection { .leftToRight }
    override var isVertical: Bool { false }
}
