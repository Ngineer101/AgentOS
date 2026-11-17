import Foundation

// A small block-level Markdown parser for agent replies (CommonMark/GFM
// flavoured, lenient). It splits the source into blocks; inline syntax
// (emphasis, code spans, links) is left in each block's text for
// `AttributedString(markdown:)` to interpret. Pure Foundation, so it can be
// unit-tested off-device.
//
// Every line is visited a bounded number of times per nesting level, so
// re-parsing a reply on each streamed chunk stays cheap.

/// One block of a Markdown document. Text payloads are inline Markdown.
enum MarkdownBlock: Equatable {
    case heading(level: Int, text: String)
    /// Soft line breaks are joined with a space; hard breaks (two trailing
    /// spaces or a trailing backslash) become "\n".
    case paragraph(String)
    /// `closed` is false while a fence is still open (a reply streaming in).
    case code(language: String?, code: String, closed: Bool)
    case quote([MarkdownBlock])
    case list(MarkdownList)
    case table(MarkdownTable)
    case rule
}

struct MarkdownList: Equatable {
    var ordered: Bool
    /// The first item's number (ordered lists only).
    var start: Int
    var items: [MarkdownListItem]
}

struct MarkdownListItem: Equatable {
    /// `nil` for a plain item; otherwise a task item, checked or not.
    var task: Bool?
    var blocks: [MarkdownBlock]
}

enum MarkdownColumnAlignment: Equatable {
    case none, left, center, right
}

struct MarkdownTable: Equatable {
    var alignments: [MarkdownColumnAlignment]
    var header: [String]
    /// Every row has exactly `header.count` cells.
    var rows: [[String]]
}

enum MarkdownParser {
    /// Parse `text` into blocks. With `streaming`, inline markers left open
    /// at the very end (`**`, `~~`, a code span) are closed so the partial
    /// reply does not flash raw syntax.
    static func parse(_ text: String, streaming: Bool = false) -> [MarkdownBlock] {
        var blocks = parseBlocks(splitLines(text))
        if streaming, !blocks.isEmpty {
            closeLast(&blocks)
        }
        return blocks
    }

    /// Agent text is untrusted: only web and mail links may be opened.
    static func isAllowedLink(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased() else { return false }
        return scheme == "http" || scheme == "https" || scheme == "mailto"
    }

    /// Close a code span, `**` or `~~` left open at the end of `s`.
    static func closingDanglingInline(_ s: String) -> String {
        let chars = Array(s)
        var i = 0
        var codeRun = 0          // length of the open code span's backtick run
        var strong = false
        var strike = false
        while i < chars.count {
            let c = chars[i]
            if c == "\\" && codeRun == 0 {
                i += 2
                continue
            }
            if c == "`" {
                var n = 0
                while i < chars.count && chars[i] == "`" { n += 1; i += 1 }
                if codeRun == 0 { codeRun = n } else if n == codeRun { codeRun = 0 }
                continue
            }
            if codeRun == 0 && i + 1 < chars.count && chars[i + 1] == c && (c == "*" || c == "_" || c == "~") {
                var n = 0
                while i < chars.count && chars[i] == c { n += 1; i += 1 }
                if n >= 2 {
                    if c == "~" { strike.toggle() } else if c == "*" { strong.toggle() }
                }
                continue
            }
            i += 1
        }
        var out = s
        if codeRun > 0 {
            out += String(repeating: "`", count: codeRun)
        } else {
            // A trailing opener with nothing after it is dropped, not closed.
            if strong {
                if out.hasSuffix("**") { out.removeLast(2) } else { out += "**" }
            }
            if strike {
                if out.hasSuffix("~~") { out.removeLast(2) } else { out += "~~" }
            }
        }
        return out
    }

    // MARK: - Lines

    private static func splitLines(_ text: String) -> [String] {
        var lines: [String] = []
        // "\r\n" is a single Character, so match it explicitly.
        let parts = text.split(omittingEmptySubsequences: false) { $0 == "\n" || $0 == "\r\n" || $0 == "\r" }
        for raw in parts {
            lines.append(expandLeadingTabs(raw))
        }
        return lines
    }

    private static func expandLeadingTabs(_ line: Substring) -> String {
        guard line.first == "\t" || (line.first == " " && line.contains("\t")) else { return String(line) }
        var col = 0
        var idx = line.startIndex
        while idx < line.endIndex {
            let c = line[idx]
            if c == " " { col += 1 } else if c == "\t" { col += 4 - col % 4 } else { break }
            idx = line.index(after: idx)
        }
        return String(repeating: " ", count: col) + line[idx...]
    }

    private static func leadingSpaces(_ line: String) -> Int {
        var n = 0
        for c in line.utf8 {
            if c == 0x20 { n += 1 } else { break }
        }
        return n
    }

    private static func isBlank(_ line: String) -> Bool {
        line.allSatisfy { $0 == " " || $0 == "\t" }
    }

    private static func stripIndent(_ line: String, _ n: Int) -> String {
        let k = min(n, leadingSpaces(line))
        return k == 0 ? line : String(line.dropFirst(k))
    }

    private static func trimmedLeading(_ line: String) -> Substring {
        line.drop { $0 == " " || $0 == "\t" }
    }

    private static func trimmed(_ s: Substring) -> String {
        s.trimmingCharacters(in: .whitespaces)
    }

    // MARK: - Block recognisers

    private struct Fence {
        var char: Character
        var count: Int
        var indent: Int
        var language: String?
    }

    private static func fenceOpen(_ line: String) -> Fence? {
        let indent = leadingSpaces(line)
        let t = line.dropFirst(indent)
        guard let c = t.first, c == "`" || c == "~" else { return nil }
        let run = t.prefix { $0 == c }.count
        guard run >= 3 else { return nil }
        let info = trimmed(t.dropFirst(run))
        if c == "`" && info.contains("`") { return nil }
        let lang = info.split(whereSeparator: { $0 == " " || $0 == "\t" }).first.map(String.init)
        return Fence(char: c, count: run, indent: indent, language: lang)
    }

    private static func isFenceClose(_ line: String, _ f: Fence) -> Bool {
        let t = trimmedLeading(line)
        let run = t.prefix { $0 == f.char }.count
        return run >= f.count && trimmed(t.dropFirst(run)).isEmpty
    }

    private static func heading(_ line: String) -> (Int, String)? {
        let t = trimmedLeading(line)
        let level = t.prefix { $0 == "#" }.count
        guard level >= 1 && level <= 6 else { return nil }
        let rest = t.dropFirst(level)
        guard rest.isEmpty || rest.first == " " || rest.first == "\t" else { return nil }
        var text = trimmed(rest)
        // Optional closing sequence: "## Title ##".
        if let lastNonHash = text.lastIndex(where: { $0 != "#" }) {
            let tail = text[text.index(after: lastNonHash)...]
            if !tail.isEmpty && (text[lastNonHash] == " " || text[lastNonHash] == "\t") {
                text = trimmed(text[...lastNonHash])
            }
        } else {
            text = ""
        }
        return (level, text)
    }

    private static func isRule(_ line: String) -> Bool {
        let t = trimmedLeading(line)
        guard let c = t.first, c == "-" || c == "*" || c == "_" else { return false }
        var n = 0
        for ch in t {
            if ch == c { n += 1 } else if ch != " " && ch != "\t" { return false }
        }
        return n >= 3
    }

    /// `===` / `---` under a paragraph (at least three, to avoid flashing a
    /// heading while a list item streams in).
    private static func setextLevel(_ line: String) -> Int? {
        let t = trimmed(trimmedLeading(line)[...])
        guard t.count >= 3, let c = t.first, c == "=" || c == "-" else { return nil }
        guard t.allSatisfy({ $0 == c }) else { return nil }
        return c == "=" ? 1 : 2
    }

    private static func isQuote(_ line: String) -> Bool {
        trimmedLeading(line).first == ">"
    }

    private struct ListMarker {
        var ordered: Bool
        var number: Int
        var indent: Int
        var contentIndent: Int
        var content: String
    }

    private static func listMarker(_ line: String) -> ListMarker? {
        let indent = leadingSpaces(line)
        let t = line.dropFirst(indent)
        guard let first = t.first else { return nil }
        var ordered = false
        var number = 1
        var width = 0
        if first == "-" || first == "*" || first == "+" {
            width = 1
        } else if first.isASCII && first.isNumber {
            let digits = t.prefix { $0.isASCII && $0.isNumber }
            guard digits.count <= 9 else { return nil }
            let after = t.dropFirst(digits.count)
            guard let d = after.first, d == "." || d == ")" else { return nil }
            ordered = true
            number = Int(digits) ?? 1
            width = digits.count + 1
        } else {
            return nil
        }
        let rest = t.dropFirst(width)
        if rest.isEmpty {
            return ListMarker(ordered: ordered, number: number, indent: indent,
                              contentIndent: indent + width + 1, content: "")
        }
        guard rest.first == " " || rest.first == "\t" else { return nil }
        let spaces = rest.prefix { $0 == " " }.count
        let content = rest.drop { $0 == " " || $0 == "\t" }
        let pad = (spaces >= 1 && spaces <= 4 && !content.isEmpty) ? spaces : 1
        return ListMarker(ordered: ordered, number: number, indent: indent,
                          contentIndent: indent + width + pad, content: String(content))
    }

    /// A list item may interrupt a paragraph only if it has content and, if
    /// ordered, starts at 1 (CommonMark), so "in 2023. Then" stays text.
    private static func listCanInterrupt(_ m: ListMarker) -> Bool {
        !m.content.isEmpty && (!m.ordered || m.number == 1)
    }

    private static func splitRow(_ line: String) -> [String] {
        var s = trimmed(line[...])
        if s.hasPrefix("|") { s.removeFirst() }
        if s.hasSuffix("|") && !s.hasSuffix("\\|") { s.removeLast() }
        var cells: [String] = []
        var cur = ""
        var escaped = false
        for c in s {
            if escaped {
                cur.append(c)
                escaped = false
            } else if c == "\\" {
                cur.append(c)
                escaped = true
            } else if c == "|" {
                cells.append(trimmed(cur[...]))
                cur = ""
            } else {
                cur.append(c)
            }
        }
        cells.append(trimmed(cur[...]))
        return cells
    }

    private static func delimiterRow(_ line: String) -> [MarkdownColumnAlignment]? {
        guard line.contains("|") || line.contains("-") else { return nil }
        let t = trimmed(line[...])
        guard t.allSatisfy({ $0 == "|" || $0 == "-" || $0 == ":" || $0 == " " || $0 == "\t" }),
              t.contains("-") else { return nil }
        var out: [MarkdownColumnAlignment] = []
        for cell in splitRow(line) {
            guard !cell.isEmpty else { return nil }
            let left = cell.hasPrefix(":")
            let right = cell.hasSuffix(":")
            let dashes = cell.drop { $0 == ":" }.reversed().drop { $0 == ":" }
            guard !dashes.isEmpty, dashes.allSatisfy({ $0 == "-" }) else { return nil }
            out.append(left && right ? .center : right ? .right : left ? .left : .none)
        }
        return out
    }

    /// Whether `lines[i]` starts a table (header row plus delimiter row).
    private static func tableStart(_ lines: [String], _ i: Int) -> [MarkdownColumnAlignment]? {
        guard i + 1 < lines.count, lines[i].contains("|"), lines[i + 1].contains("|") else { return nil }
        guard let aligns = delimiterRow(lines[i + 1]) else { return nil }
        return splitRow(lines[i]).count == aligns.count ? aligns : nil
    }

    /// A line that starts some block other than a paragraph (ends lazy
    /// continuation and interrupts paragraphs).
    private static func startsBlock(_ line: String) -> Bool {
        if fenceOpen(line) != nil || heading(line) != nil || isRule(line) || isQuote(line) { return true }
        if let m = listMarker(line), listCanInterrupt(m) { return true }
        return false
    }

    // MARK: - Block parsing

    private static func parseBlocks(_ lines: [String]) -> [MarkdownBlock] {
        var out: [MarkdownBlock] = []
        var para: [String] = []
        var i = 0

        func flush() {
            if !para.isEmpty {
                out.append(.paragraph(joinParagraph(para)))
                para.removeAll()
            }
        }

        while i < lines.count {
            let line = lines[i]
            if isBlank(line) {
                flush()
                i += 1
                continue
            }
            if let fence = fenceOpen(line) {
                flush()
                i = parseFence(lines, i, fence, into: &out)
                continue
            }
            if let h = heading(line) {
                flush()
                out.append(.heading(level: h.0, text: h.1))
                i += 1
                continue
            }
            if !para.isEmpty, let level = setextLevel(line) {
                let text = joinParagraph(para)
                para.removeAll()
                out.append(.heading(level: level, text: text))
                i += 1
                continue
            }
            if isRule(line) {
                flush()
                out.append(.rule)
                i += 1
                continue
            }
            if isQuote(line) {
                flush()
                i = parseQuote(lines, i, into: &out)
                continue
            }
            if let m = listMarker(line), para.isEmpty || listCanInterrupt(m) {
                flush()
                i = parseList(lines, i, m, into: &out)
                continue
            }
            if let aligns = tableStart(lines, i) {
                flush()
                i = parseTable(lines, i, aligns, into: &out)
                continue
            }
            para.append(line)
            i += 1
        }
        flush()
        return out
    }

    private static func joinParagraph(_ lines: [String]) -> String {
        var s = ""
        for (k, raw) in lines.enumerated() {
            var line = String(trimmedLeading(raw))
            var hard = false
            if line.hasSuffix("  ") {
                hard = true
            }
            while line.last == " " || line.last == "\t" { line.removeLast() }
            if line.hasSuffix("\\") && !line.hasSuffix("\\\\") {
                hard = true
                line.removeLast()
            }
            s += line
            if k < lines.count - 1 {
                s += hard ? "\n" : " "
            }
        }
        return s
    }

    private static func parseFence(_ lines: [String], _ start: Int, _ f: Fence,
                                   into out: inout [MarkdownBlock]) -> Int {
        var body: [String] = []
        var j = start + 1
        var closed = false
        while j < lines.count {
            if isFenceClose(lines[j], f) {
                closed = true
                j += 1
                break
            }
            body.append(stripIndent(lines[j], f.indent))
            j += 1
        }
        if !closed {
            // Still streaming: hide a half-typed closing fence and the
            // empty line after the last newline.
            if let last = body.last {
                let t = trimmedLeading(last)
                if !t.isEmpty && t.allSatisfy({ $0 == f.char }) { body.removeLast() }
            }
            while let last = body.last, isBlank(last) { body.removeLast() }
        }
        out.append(.code(language: f.language, code: body.joined(separator: "\n"), closed: closed))
        return j
    }

    private static func parseQuote(_ lines: [String], _ start: Int,
                                   into out: inout [MarkdownBlock]) -> Int {
        var inner: [String] = []
        var j = start
        while j < lines.count {
            let line = lines[j]
            let t = trimmedLeading(line)
            if t.first == ">" {
                var rest = t.dropFirst()
                if rest.first == " " { rest = rest.dropFirst() }
                inner.append(expandLeadingTabs(rest))
            } else if !isBlank(line), let last = inner.last, !isBlank(last), !startsBlock(line) {
                inner.append(line)   // lazy continuation of a quoted paragraph
            } else {
                break
            }
            j += 1
        }
        out.append(.quote(parseBlocks(inner)))
        return j
    }

    private static func parseList(_ lines: [String], _ start: Int, _ first: ListMarker,
                                  into out: inout [MarkdownBlock]) -> Int {
        var items: [MarkdownListItem] = []
        var marker = first
        var body: [String] = [first.content]
        var j = start + 1

        func finishItem() {
            while let last = body.last, isBlank(last) { body.removeLast() }
            var task: Bool?
            if let head = body.first {
                if head == "[ ]" || head.hasPrefix("[ ] ") {
                    task = false
                } else if head == "[x]" || head == "[X]" || head.hasPrefix("[x] ") || head.hasPrefix("[X] ") {
                    task = true
                }
                if task != nil {
                    body[0] = String(head.dropFirst(3).drop { $0 == " " })
                }
            }
            items.append(MarkdownListItem(task: task, blocks: parseBlocks(body)))
        }

        while j < lines.count {
            let line = lines[j]
            if isBlank(line) {
                body.append("")
                j += 1
                continue
            }
            let indent = leadingSpaces(line)
            if indent >= marker.contentIndent {
                body.append(stripIndent(line, marker.contentIndent))
                j += 1
                continue
            }
            if isRule(line) { break }
            if let m = listMarker(line) {
                if indent > marker.indent {
                    // A sub-list indented less than this item's content.
                    body.append(stripIndent(line, indent))
                    j += 1
                    continue
                }
                if m.ordered == first.ordered {
                    finishItem()
                    marker = m
                    body = [m.content]
                    j += 1
                    continue
                }
                break
            }
            if let last = body.last, !isBlank(last), !startsBlock(line), tableStart(lines, j) == nil {
                body.append(String(trimmedLeading(line)))   // lazy continuation
                j += 1
                continue
            }
            break
        }
        finishItem()
        out.append(.list(MarkdownList(ordered: first.ordered, start: first.number, items: items)))
        return j
    }

    private static func parseTable(_ lines: [String], _ start: Int, _ aligns: [MarkdownColumnAlignment],
                                   into out: inout [MarkdownBlock]) -> Int {
        let width = aligns.count
        func fit(_ cells: [String]) -> [String] {
            if cells.count == width { return cells }
            if cells.count > width { return Array(cells.prefix(width)) }
            return cells + Array(repeating: "", count: width - cells.count)
        }
        let header = fit(splitRow(lines[start]))
        var rows: [[String]] = []
        var j = start + 2
        while j < lines.count {
            let line = lines[j]
            if isBlank(line) || !line.contains("|") || fenceOpen(line) != nil || isQuote(line) { break }
            rows.append(fit(splitRow(line)))
            j += 1
        }
        out.append(.table(MarkdownTable(alignments: aligns, header: header, rows: rows)))
        return j
    }

    // MARK: - Streaming

    private static func closeLast(_ blocks: inout [MarkdownBlock]) {
        guard let last = blocks.popLast() else { return }
        switch last {
        case let .paragraph(s):
            blocks.append(.paragraph(closingDanglingInline(s)))
        case let .heading(level, s):
            blocks.append(.heading(level: level, text: closingDanglingInline(s)))
        case var .quote(inner):
            closeLast(&inner)
            blocks.append(.quote(inner))
        case var .list(list):
            if var item = list.items.popLast() {
                closeLast(&item.blocks)
                list.items.append(item)
            }
            blocks.append(.list(list))
        case var .table(table):
            if var row = table.rows.popLast() {
                if let cell = row.popLast() { row.append(closingDanglingInline(cell)) }
                table.rows.append(row)
            }
            blocks.append(.table(table))
        case .code, .rule:
            blocks.append(last)
        }
    }
}
