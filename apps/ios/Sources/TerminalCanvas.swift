import UIKit

final class TerminalContainer: UIView {
    let canvas = TerminalCanvas()
    let input = TerminalInput()
    let selection = TerminalSelectionView()
    var onSize: ((NativeViewport) -> Void)?
    var onPointer: ((TerminalPointer) -> Void)?
    var onGeometryInvalidated: (() -> Void)?
    private(set) lazy var gestures = TerminalGestures(view: self)
    var cellSize: CGSize { CGSize(width: canvas.cellWidth, height: canvas.cellHeight) }
    var gesturesReady: Bool {
        !interactionActive && !selection.hasSelection && !transitioning
            && latestFrame?.state == "active"
            && TerminalGeometry.viewport(bounds.size, cell: cellSize) == canvas.nativeFrame?.viewport
    }
    private var transitioning = false
    private var transitionEnd: DispatchWorkItem?
    private var rotation: ObjectIdentifier?
    private var previousBounds: CGSize = .zero
    var onSelectionError: (() -> Void)?
    var onHistoryError: ((String) -> Void)?
    let latestButton = UIButton(type: .system)
    private var measured: NativeViewport?
    private var submitted: NativeViewport?
    private(set) var selectionController: TerminalSelectionController?
    private struct Page {
        let source: NativeFrameSource
        let first: Int64
        let count: Int
        let height: Int
        let generation: UInt64
        func endpoint(_ cell: TerminalTextDocument.Cell) -> TerminalSelectionController.Endpoint? {
            guard cell.row >= first, cell.row < first + Int64(count), count >= height else { return nil }
            let top = max(first, min(cell.row, first + Int64(count - height)))
            guard let viewport = try? source.viewportSource(firstRow: top) else { return nil }
            return .init(source: viewport, row: UInt16(cell.row - top), column: UInt16(cell.column), logicalRow: cell.row)
        }
    }
    private var pages: [Page] = []
    private var interactionActive = false
    private var keyboardRequested = false
    private var keyboardActive = false
    private var latestFrame: NativeFrame?
    private var selectionWidth: CGFloat?
    private var pendingEdgeScroll: (first: Int64, started: TimeInterval)?
    private var lastHistoryNotice: String?
    private var strings = AppStrings()

    init() {
        super.init(frame: .zero)
        clipsToBounds = true
        addSubview(canvas)
        addSubview(selection)
        addSubview(input)
        var config: UIButton.Configuration
        if #available(iOS 26.0, *) {
            config = .glass()
        } else {
            config = .filled()
            config.baseBackgroundColor = .secondarySystemGroupedBackground
            config.baseForegroundColor = .label
            config.background.strokeColor = .separator.withAlphaComponent(0.18)
            config.background.strokeWidth = 0.5
            latestButton.layer.shadowColor = UIColor.black.cgColor
            latestButton.layer.shadowOpacity = 0.14
            latestButton.layer.shadowRadius = 5
            latestButton.layer.shadowOffset = CGSize(width: 0, height: 2)
        }
        config.image = UIImage(systemName: "arrow.down")
        config.preferredSymbolConfigurationForImage = UIImage.SymbolConfiguration(pointSize: 18, weight: .semibold)
        config.contentInsets = .zero
        config.cornerStyle = .capsule
        latestButton.configuration = config
        latestButton.accessibilityLabel = strings("回到最新输出")
        latestButton.accessibilityIdentifier = "terminalLatest"
        latestButton.addTarget(self, action: #selector(returnToLatest), for: .touchUpInside)
        latestButton.isHidden = true
        addSubview(latestButton)
        _ = gestures
        selection.onTouch = { [weak self] in self?.gestures.touch($0) }
        input.onCompositionChange = { [weak self] in self?.setNeedsLayout() }
        selection.onBegin = { [weak self] in
            self?.interactionActive = true
            self?.invalidateCoordinates()
        }
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardWillChange(_:)), name: UIResponder.keyboardWillChangeFrameNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardDidChange), name: UIResponder.keyboardDidChangeFrameNotification, object: nil)
        selection.onEnd = { [weak self] in
            self?.interactionActive = false
            self?.setNeedsLayout()
        }
        selection.onSelection = { [weak self] range in self?.select(range) }
        selection.onCopy = { [weak self] in self?.selectionController?.copy() }
        selection.hyperlink = { [weak self] in await self?.selectionController?.hyperlink() }
        selection.onOpenLink = { [weak self] url in
            Task { @MainActor [weak self] in
                guard let self, let controller = self.selectionController,
                      await controller.hyperlink() == url, self.selectionController === controller else { return }
                if await UIApplication.shared.open(url) { self.selection.clear() }
                else { self.onHistoryError?(self.strings("无法打开链接。")) }
            }
        }
        selection.onEdgeTick = { [weak self] in self?.edgeScroll() }
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func setLanguage(_ language: String) {
        strings = AppStrings(language: language)
        latestButton.accessibilityLabel = strings("回到最新输出")
        input.accessibilityLabel = strings("终端输入")
        canvas.accessibilityLabel = strings("远程终端")
        selection.openLinkTitle = strings("打开链接")
    }

    func setFontSize(_ size: CGFloat) {
        guard canvas.font.pointSize != size else { return }
        invalidateCoordinates()
        selection.clear()
        measured = nil
        canvas.font = TerminalFont.font(size: size)
        setNeedsLayout()
        canvas.setNeedsDisplay()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        if previousBounds != bounds.size {
            previousBounds = bounds.size
            invalidateCoordinates()
        }
        if let coordinator = window?.rootViewController?.transitionCoordinator, coordinator.isAnimated {
            let identity = ObjectIdentifier(coordinator as AnyObject)
            if rotation != identity {
                rotation = identity
                beginTransition(duration: coordinator.transitionDuration)
                coordinator.animate(alongsideTransition: nil) { [weak self] _ in
                    self?.rotation = nil
                    self?.finishTransition()
                }
            }
        }
        canvas.frame = bounds
        selection.frame = bounds
        selection.cellSize = CGSize(width: canvas.cellWidth, height: canvas.cellHeight)
        input.font = canvas.font
        let cursor = canvas.nativeFrame
        let x = min(max(0, bounds.width - cellSize.width), CGFloat(cursor?.cursorColumn ?? 0) * cellSize.width)
        let y = min(max(0, bounds.height - cellSize.height), CGFloat(cursor?.cursorRow ?? 0) * cellSize.height)
        input.frame = CGRect(x: x, y: y, width: max(cellSize.width, bounds.width - x), height: cellSize.height)
        latestButton.frame = CGRect(x: (bounds.width - 44) / 2, y: max(0, bounds.height - 56),
                                    width: 44, height: 44)
        guard bounds.width > 0, bounds.height > 0 else { return }
        if let width = selectionWidth, abs(width - bounds.width) > 1 { selection.clear() }
        // Dismissing the keyboard for reading must not resize/reflow the captured text.
        guard !interactionActive, !selection.hasSelection, !transitioning,
              let size = TerminalGeometry.viewport(bounds.size, cell: cellSize) else { return }
        if size != measured || size != submitted {
            if size != measured { measured = size; invalidateCoordinates() }
            // Layout callbacks must not publish SwiftUI state during updateUIView.
            DispatchQueue.main.async { [weak self] in
                guard let self, self.measured == size, self.submitted != size, !self.transitioning else { return }
                self.submitted = size
                self.onSize?(size)
            }
        }
    }

    func update(_ frame: NativeFrame?, terminal: NativeTerminal?) {
        if selectionController?.terminal !== terminal {
            invalidateCoordinates()
            selection.clear()
            selectionController?.select(nil)
            selectionController = terminal.map(TerminalSelectionController.init)
            selectionController?.onFailure = { [weak self] in
                self?.selection.clear()
                self?.onSelectionError?()
            }
            selectionController?.onScrollFailure = { [weak self] in
                guard let self else { return }
                self.historyFailed(self.strings("历史内容暂时无法加载，请稍后重试。"))
            }
        }
        latestFrame = frame
        if frame?.notice == nil { lastHistoryNotice = nil }
        input.admission(ready: frame?.inputReady == true, epoch: frame?.inputEpoch)
        selection.isUserInteractionEnabled = frame?.state == "active" && frame?.source != nil
        if frame?.inputEpoch != canvas.nativeFrame?.inputEpoch || frame?.geometryGeneration != canvas.nativeFrame?.geometryGeneration
            || frame?.activeScreen != canvas.nativeFrame?.activeScreen || frame?.state != "active" {
            invalidateCoordinates()
            selection.clear()
        }
        gestures.update(frame)
        updateLatestButton()
        if selection.hasSelection {
            guard let frame, frame.selection != nil, frame.source != nil else { return }
            if ["selection_changed", "resource_limit", "history_unavailable"].contains(frame.notice ?? "") {
                if lastHistoryNotice != frame.notice {
                    lastHistoryNotice = frame.notice
                    historyFailed(strings("历史内容发生变化或暂时不可用，请重新选择。"))
                }
                return
            }
            let first = frame.firstRow + Int64(frame.windowOffset - frame.historyOffset)
            guard first != canvas.visibleFirstRow else { return }
            // Resolve the new window only after the shared driver has supplied it.
            let rows = frame.source!.presentationRows()
            remember(frame, count: rows.count)
            do {
                canvas.update(frame)
                try selection.show(rows: rows, firstRow: frame.firstRow, visibleFirst: first)
                trimPages()
                pendingEdgeScroll = nil
            } catch {
                selection.clear()
                onHistoryError?(strings(error is TerminalTextDocument.MergeError ? "选区过大或历史内容已变化，请分段重新选择。" : "无法扩展选区，请重新选择。"))
            }
            return
        }
        if TerminalGeometry.retainDisplayed(canvas.nativeFrame, candidate: frame, target: measured, transitioning: transitioning) { return }
        canvas.update(frame)
        selection.update(rows: canvas.visibleRows, firstRow: canvas.visibleFirstRow)
    }

    func setKeyboard(_ visible: Bool, active: Bool = true) {
        let opening = visible && !keyboardRequested
        keyboardRequested = visible
        keyboardActive = active
        input.setSoftwareKeyboard(visible)
        if opening {
            selection.clear()
            if selection.isFirstResponder { selection.resignFirstResponder() }
        }
        reconcileKeyboard()
    }

    private func reconcileKeyboard() {
        guard keyboardActive else {
            if input.isFirstResponder { input.resignFirstResponder() }
            return
        }
        guard window != nil, !interactionActive, !selection.hasSelection, !selection.isFirstResponder,
              window?.rootViewController?.presentedViewController == nil else { return }
        if !input.isFirstResponder { input.becomeFirstResponder() }
    }

    private func select(_ range: NSRange?) {
        canvas.isSelecting = range != nil
        guard let range else {
            pages = []
            pendingEdgeScroll = nil
            selectionWidth = nil
            selectionController?.select(nil)
            canvas.update(latestFrame)
            updateLatestButton()
            setNeedsLayout()
            return
        }
        if let frame = canvas.nativeFrame { remember(frame, count: canvas.presentationRowCount) }
        guard let endpoints = selection.document.endpoints(range),
              let first = pages.reversed().lazy.compactMap({ $0.endpoint(endpoints.0) }).first,
              let last = pages.reversed().lazy.compactMap({ $0.endpoint(endpoints.1) }).first else {
            selection.clear()
            onSelectionError?()
            return
        }
        selectionWidth = bounds.width
        selectionController?.select(.init(first: first, last: last))
        updateLatestButton()
    }

    private func remember(_ frame: NativeFrame, count: Int) {
        guard let source = frame.source,
              !pages.contains(where: { $0.generation == frame.contentGeneration && $0.first == frame.firstRow }) else { return }
        pages.append(Page(source: source, first: frame.firstRow, count: count, height: Int(frame.viewport.rows), generation: frame.contentGeneration))
    }

    private func trimPages() {
        let document = selection.document
        pages.removeAll { $0.first + Int64($0.count) <= document.firstRow || $0.first >= document.firstRow + Int64(document.rowCount) }
    }

    private func edgeScroll() {
        guard selection.hasSelection, let point = selection.dragPoint, let frame = latestFrame,
              frame.state == "active", frame.activeScreen == .main else { return }
        if let pending = pendingEdgeScroll {
            if Date.timeIntervalSinceReferenceDate - pending.started > 5 {
                historyFailed(strings("历史内容加载超时，请稍后重新拖动。"))
            }
            return
        }
        let direction = TerminalEdgeScroll.direction(point: point, bounds: selection.bounds)
        guard let offset = TerminalEdgeScroll.target(offset: frame.historyOffset, maximum: frame.historyMaximum, direction: direction) else { return }
        pendingEdgeScroll = (canvas.visibleFirstRow, Date.timeIntervalSinceReferenceDate)
        selectionController?.scroll(offset: offset, older: direction > 0)
    }

    private func historyFailed(_ message: String) {
        gestures.cancel()
        selection.stopTracking()
        pendingEdgeScroll = nil
        onHistoryError?(message)
    }

    private func updateLatestButton() {
        latestButton.isHidden = (latestFrame?.historyOffset ?? 0) == 0 && !selection.hasSelection
        latestButton.isEnabled = latestFrame?.state == "active"
    }

    @objc func returnToLatest() {
        gestures.cancel()
        selection.clear()
        pendingEdgeScroll = nil
        selectionController?.scroll(offset: 0, older: false)
        reconcileKeyboard()
    }

    func invalidateCoordinates() {
        gestures.cancel()
        onGeometryInvalidated?()
    }

    @objc private func keyboardWillChange(_ notification: Notification) {
        guard window != nil else { return }
        beginTransition(duration: (notification.userInfo?[UIResponder.keyboardAnimationDurationUserInfoKey] as? Double) ?? 0.25)
    }

    @objc private func keyboardDidChange() {
        guard rotation == nil else { return }
        finishTransition()
    }

    private func beginTransition(duration: TimeInterval) {
        transitioning = true
        invalidateCoordinates()
        transitionEnd?.cancel()
        let end = DispatchWorkItem { [weak self] in self?.finishTransition() }
        transitionEnd = end
        // Covers interrupted keyboard transitions that omit the did-change notification.
        DispatchQueue.main.asyncAfter(deadline: .now() + max(0, duration) + 0.05, execute: end)
    }

    private func finishTransition() {
        transitionEnd?.cancel(); transitionEnd = nil
        transitioning = false
        setNeedsLayout()
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window == nil { transitionEnd?.cancel(); transitionEnd = nil; transitioning = false; invalidateCoordinates() }
        else { DispatchQueue.main.async { [weak self] in self?.reconcileKeyboard() } }
    }

}

final class TerminalCanvas: UIView {
    private(set) var nativeFrame: NativeFrame?
    var isSelecting = false { didSet { updateCursorBlink(); setNeedsDisplay() } }
    private(set) var cursorPhaseVisible = true
    private var blink: Timer?
    private var applicationActive = true
    private var rows: [NativeRow] = []
    private var source: NativeFrameSource?
    private var contentGeneration: UInt64?
    var font = TerminalFont.font(size: 15) {
        didSet { measuredCellWidth = TerminalFont.advance("M", font: font) }
    }
    private var measuredCellWidth = TerminalFont.advance("M", font: TerminalFont.font(size: 15))
    var cellWidth: CGFloat { measuredCellWidth }
    var cellHeight: CGFloat { ceil(font.lineHeight + 2) }
    var visibleFirstRow: Int64 {
        guard let frame = nativeFrame else { return 0 }
        return frame.firstRow + Int64(frame.windowOffset >= frame.historyOffset ? frame.windowOffset - frame.historyOffset : 0)
    }
    var presentationRowCount: Int { rows.count }
    var visibleRows: [NativeRow] {
        guard let frame = nativeFrame else { return [] }
        let start = Int(frame.windowOffset >= frame.historyOffset ? frame.windowOffset - frame.historyOffset : 0)
        return Array(rows.dropFirst(start).prefix(Int(frame.viewport.rows)))
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isOpaque = true
        contentMode = .redraw
        accessibilityLabel = "远程终端"
        accessibilityIdentifier = "terminalCanvas"
        NotificationCenter.default.addObserver(self, selector: #selector(suspendBlink), name: UIApplication.willResignActiveNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(resumeBlink), name: UIApplication.didBecomeActiveNotification, object: nil)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func update(_ frame: NativeFrame?) {
        if let next = frame?.source {
            if source == nil || contentGeneration != frame?.contentGeneration {
                let oldRows = rows
                rows = next.presentationRowsFrom(previous: source).map { update in
                    switch update {
                    case .reuse(let index): return oldRows[Int(index)]
                    case .replace(let row): return row
                    }
                }
                source = next
                contentGeneration = frame?.contentGeneration
            }
        } else {
            source = nil
            contentGeneration = nil
            rows = frame?.rows ?? []
        }
        let reset = nativeFrame?.cursorRow != frame?.cursorRow || nativeFrame?.cursorColumn != frame?.cursorColumn
            || nativeFrame?.cursorShape != frame?.cursorShape || nativeFrame?.cursorBlinking != frame?.cursorBlinking
            || nativeFrame?.cursorVisible != frame?.cursorVisible
        nativeFrame = frame
        updateCursorBlink(reset: reset)
        setNeedsDisplay()
    }

    private func updateCursorBlink(reset: Bool = false) {
        let enabled = window != nil && applicationActive && !isHidden && !isSelecting && nativeFrame?.state == "active"
            && nativeFrame?.cursorVisible == true && nativeFrame?.cursorBlinking == true && nativeFrame?.historyOffset == 0
        if reset || !enabled { blink?.invalidate(); blink = nil; cursorPhaseVisible = true }
        if enabled && blink == nil {
            let timer = Timer(timeInterval: 0.5, repeats: true) { [weak self] timer in
                guard self != nil else { timer.invalidate(); return }
                MainActor.assumeIsolated {
                    self?.cursorPhaseVisible.toggle()
                    self?.setNeedsDisplay()
                }
            }
            blink = timer
            RunLoop.main.add(timer, forMode: .common)
        }
    }

    @objc private func suspendBlink() { applicationActive = false; updateCursorBlink(); setNeedsDisplay() }
    @objc private func resumeBlink() { applicationActive = true; updateCursorBlink(reset: true); setNeedsDisplay() }
    override func didMoveToWindow() { super.didMoveToWindow(); updateCursorBlink(reset: true) }
    override var isHidden: Bool { didSet { updateCursorBlink() } }

    /// A cursor reported on the continuation of a wide glyph owns both cells.
    var cursorRect: CGRect? {
        guard let frame = nativeFrame, frame.cursorRow < frame.viewport.rows, frame.cursorColumn < frame.viewport.columns else { return nil }
        let index = Int(frame.windowOffset) + Int(frame.cursorRow)
        let cells = rows.indices.contains(index) ? rows[index].cells : []
        var column = Int(frame.cursorColumn)
        if cells.indices.contains(column), cells[column].width == 0, column > 0, cells[column - 1].width == 2 { column -= 1 }
        let width = cells.indices.contains(column) && cells[column].width == 2 ? cellWidth * 2 : cellWidth
        var box = CGRect(x: CGFloat(column) * cellWidth, y: CGFloat(frame.cursorRow) * cellHeight, width: width, height: cellHeight)
        switch frame.cursorShape {
        case .beam: box.size.width = 2
        case .underline: box.origin.y = box.maxY - 2; box.size.height = 2
        case .block: break
        }
        return box
    }

    override func draw(_ rect: CGRect) {
        guard let context = UIGraphicsGetCurrentContext() else { return }
        color(nativeFrame?.background ?? (traitCollection.userInterfaceStyle == .dark ? 0xff101713 : 0xfff5f8f3)).setFill()
        context.fill(bounds)
        guard let frame = nativeFrame else { return }
        let faces = (0..<4).map { TerminalFont.font(size: font.pointSize, bold: $0 & 1 != 0, italic: $0 & 2 != 0) }
        let start = Int(frame.windowOffset >= frame.historyOffset ? frame.windowOffset - frame.historyOffset : 0)
        for rowIndex in 0..<min(Int(frame.viewport.rows), Int(ceil(bounds.height / cellHeight))) {
            guard rows.indices.contains(start + rowIndex) else { continue }
            let row = rows[start + rowIndex]
            // Paint backgrounds first, so a wide cluster cannot be erased by its continuation.
            context.setShouldAntialias(false)
            for (column, cell) in row.cells.enumerated() where cell.width > 0 {
                color(cell.background).setFill()
                context.fill(CGRect(x: CGFloat(column) * cellWidth, y: CGFloat(rowIndex) * cellHeight,
                                    width: CGFloat(cell.width) * cellWidth, height: cellHeight))
            }
            context.setShouldAntialias(true)
            for (column, cell) in row.cells.enumerated() where cell.width > 0 && cell.attributes & 16 == 0 {
                let box = CGRect(x: CGFloat(column) * cellWidth, y: CGFloat(rowIndex) * cellHeight,
                                 width: CGFloat(cell.width) * cellWidth, height: cellHeight)
                let face = faces[(cell.attributes & 1 != 0 ? 1 : 0) + (cell.attributes & 4 != 0 ? 2 : 0)]
                let attributes: [NSAttributedString.Key: Any] = [.font: face,
                    .foregroundColor: color(cell.foreground).withAlphaComponent(cell.attributes & 2 != 0 ? 0.6 : 1)]
                let width = TerminalFont.advance(cell.text, font: face)
                context.saveGState()
                context.clip(to: box)
                context.translateBy(x: box.minX, y: box.minY + 1)
                context.scaleBy(x: width > box.width + 0.01 ? box.width / width : 1, y: 1)
                (cell.text as NSString).draw(at: .zero, withAttributes: attributes)
                context.restoreGState()
                if cell.attributes & 8 != 0 {
                    color(cell.foreground).setFill()
                    context.fill(CGRect(x: box.minX, y: box.minY + 1 + face.ascender * 0.65, width: box.width, height: 1))
                }
                if cell.underline != 0 {
                    drawUnderline(cell, box: box, context: context)
                }
            }
        }
        if frame.cursorVisible && frame.historyOffset == 0 && !isSelecting && (!frame.cursorBlinking || cursorPhaseVisible), let box = cursorRect {
            color(frame.cursorColor).withAlphaComponent(frame.cursorShape == .block ? 0.45 : 1).setFill()
            context.fill(box)
        }
    }

    private func drawUnderline(_ cell: NativeCell, box: CGRect, context: CGContext) {
        context.saveGState()
        context.clip(to: box)
        context.setStrokeColor(color(cell.underlineColor).cgColor)
        context.setLineWidth(1)
        let y = box.maxY - 1.5
        context.move(to: CGPoint(x: box.minX, y: y))
        switch cell.underline {
        case 3:
            var x = box.minX
            while x < box.maxX {
                let end = min(x + 4, box.maxX)
                context.addQuadCurve(to: CGPoint(x: end, y: y), control: CGPoint(x: (x + end) / 2, y: y - 3))
                x = end
            }
        case 4, 5:
            context.setLineDash(phase: 0, lengths: cell.underline == 4 ? [1, 2] : [4, 2])
            context.addLine(to: CGPoint(x: box.maxX, y: y))
        default:
            context.addLine(to: CGPoint(x: box.maxX, y: y))
            if cell.underline == 2 {
                context.move(to: CGPoint(x: box.minX, y: y - 2))
                context.addLine(to: CGPoint(x: box.maxX, y: y - 2))
            }
        }
        context.strokePath()
        context.restoreGState()
    }

    private func color(_ argb: UInt32) -> UIColor {
        UIColor(red: CGFloat((argb >> 16) & 255) / 255, green: CGFloat((argb >> 8) & 255) / 255,
                blue: CGFloat(argb & 255) / 255, alpha: 1)
    }
}
