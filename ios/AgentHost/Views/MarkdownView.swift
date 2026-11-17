import SwiftUI

/// An agent reply rendered as Markdown: headings, paragraphs, lists, code
/// blocks, quotes, rules and tables, with inline emphasis, code and links.
/// `streaming` tolerates a reply that is still arriving (an open code fence,
/// an unclosed `**`). Links open only for http, https and mailto.
struct MarkdownView: View, Equatable {
    let text: String
    var streaming = false

    nonisolated static func == (lhs: MarkdownView, rhs: MarkdownView) -> Bool {
        lhs.text == rhs.text && lhs.streaming == rhs.streaming
    }

    var body: some View {
        MarkdownBlocksView(blocks: MarkdownParser.parse(text, streaming: streaming), depth: 0)
            .frame(maxWidth: .infinity, alignment: .leading)
            .textSelection(.enabled)
            .environment(\.openURL, OpenURLAction { url in
                MarkdownParser.isAllowedLink(url) ? .systemAction : .discarded
            })
    }
}

/// A sequence of blocks, stacked. `depth` is the list nesting level.
struct MarkdownBlocksView: View {
    let blocks: [MarkdownBlock]
    let depth: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(blocks.indices, id: \.self) { i in
                MarkdownBlockView(block: blocks[i], depth: depth)
            }
        }
    }
}

struct MarkdownBlockView: View {
    let block: MarkdownBlock
    let depth: Int

    var body: some View {
        switch block {
        case let .heading(level, text):
            MarkdownHeading(level: level, text: text)
        case let .paragraph(text):
            InlineMarkdownText(source: text)
        case let .code(language, code, _):
            CodeBlock(text: code, language: language, font: .footnote.monospaced())
        case let .quote(inner):
            MarkdownQuote(blocks: inner, depth: depth)
        case let .list(list):
            MarkdownListView(list: list, depth: depth)
        case let .table(table):
            MarkdownTableView(table: table)
        case .rule:
            Divider()
                .padding(.vertical, 4)
        }
    }
}

/// Inline Markdown (emphasis, strikethrough, code spans, links) as one
/// `Text`; plain text if it does not parse.
struct InlineMarkdownText: View {
    let source: String

    var body: some View {
        Text(InlineMarkdownText.attributed(source))
            .fixedSize(horizontal: false, vertical: true)
    }

    static func attributed(_ source: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        guard var out = try? AttributedString(markdown: source, options: options) else {
            return AttributedString(source)
        }
        // Code spans get a faint background, as on the desktop.
        var codeRanges: [Range<AttributedString.Index>] = []
        for run in out.runs {
            if let intent = run.inlinePresentationIntent, intent.contains(.code) {
                codeRanges.append(run.range)
            }
        }
        for range in codeRanges {
            out[range].backgroundColor = Color(uiColor: .tertiarySystemFill)
        }
        return out
    }
}

struct MarkdownHeading: View {
    let level: Int
    let text: String

    var body: some View {
        InlineMarkdownText(source: text)
            .font(headingFont)
            .padding(.top, level <= 2 ? 4 : 0)
            .accessibilityAddTraits(.isHeader)
    }

    private var headingFont: Font {
        switch level {
        case 1: return .title2.bold()
        case 2: return .title3.bold()
        case 3: return .headline
        default: return .subheadline.bold()
        }
    }
}

struct MarkdownQuote: View {
    let blocks: [MarkdownBlock]
    let depth: Int

    var body: some View {
        MarkdownBlocksView(blocks: blocks, depth: depth)
            .foregroundStyle(.secondary)
            .padding(.leading, 12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .leading) {
                RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                    .fill(Color(uiColor: .tertiaryLabel))
                    .frame(width: 3)
            }
    }
}

struct MarkdownListView: View {
    let list: MarkdownList
    let depth: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(list.items.indices, id: \.self) { i in
                MarkdownListItemView(item: list.items[i], marker: marker(i), depth: depth)
            }
        }
    }

    private func marker(_ i: Int) -> String {
        if list.ordered { return "\(list.start + i)." }
        switch depth % 3 {
        case 0: return "\u{2022}"   // •
        case 1: return "\u{25E6}"   // ◦
        default: return "\u{25AA}"  // ▪
        }
    }
}

struct MarkdownListItemView: View {
    let item: MarkdownListItem
    let marker: String
    let depth: Int

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            markerView
            MarkdownBlocksView(blocks: item.blocks, depth: depth + 1)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private var markerView: some View {
        if let done = item.task {
            Image(systemName: done ? "checkmark.square.fill" : "square")
                .foregroundStyle(done ? Color.accentColor : Color.secondary)
        } else {
            Text(marker)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
    }
}

/// A simple grid that scrolls sideways when it is wider than the screen.
struct MarkdownTableView: View {
    let table: MarkdownTable

    var body: some View {
        ScrollView(.horizontal) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(table.header.indices, id: \.self) { c in
                        MarkdownTableCell(text: table.header[c], header: true)
                            .gridColumnAlignment(alignment(c))
                    }
                }
                ForEach(table.rows.indices, id: \.self) { r in
                    Divider()
                        .gridCellUnsizedAxes(.horizontal)
                    GridRow {
                        ForEach(table.rows[r].indices, id: \.self) { c in
                            MarkdownTableCell(text: table.rows[r][c], header: false)
                        }
                    }
                }
            }
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .strokeBorder(Color(uiColor: .separator), lineWidth: 0.5)
            )
        }
    }

    private func alignment(_ c: Int) -> HorizontalAlignment {
        guard c < table.alignments.count else { return .leading }
        switch table.alignments[c] {
        case .center: return .center
        case .right: return .trailing
        case .left, .none: return .leading
        }
    }
}

struct MarkdownTableCell: View {
    let text: String
    let header: Bool

    var body: some View {
        InlineMarkdownText(source: text)
            .font(header ? Font.subheadline.weight(.semibold) : Font.subheadline)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
    }
}
