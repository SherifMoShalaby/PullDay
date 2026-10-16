import AppKit
import SwiftUI

struct LogView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        VStack(spacing: 0) {
            if state.log.isEmpty {
                Text("Nothing logged yet. Checks and updates show up here.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(state.log) { entry in
                    DisclosureGroup {
                        Text(entry.output.isEmpty ? "(no output)" : entry.output)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(8)
                            .background(Color.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 6))
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: entry.success ? "checkmark.circle.fill" : "xmark.octagon.fill")
                                .foregroundStyle(entry.success ? Color.green : Color.red)
                            Text(entry.title).lineLimit(1)
                            Spacer()
                            Text(entry.date, style: .time).foregroundStyle(.secondary).font(.caption)
                        }
                    }
                }
            }
            Divider()
            HStack {
                Button("Open Log File") { NSWorkspace.shared.open(LogFile.url) }
                Spacer()
                Button("Clear") { state.clearLog() }.disabled(state.log.isEmpty)
            }
            .padding(10)
        }
        .frame(minWidth: 560, minHeight: 380)
    }
}
