import DifuKit
import PhotosUI
import SwiftUI

/// Draft message and attachments for one session screen.
@MainActor
@Observable
final class ComposerModel {
    var text = ""
    var attachments: [PickedImage] = []
    var sending = false

    var hasContent: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty
    }
}

/// Queued messages, with remove and "Send now".
struct QueueSection: View {
    let view: SessionView
    let remove: (Int) -> Void
    let sendNow: () -> Void
    /// Opens a queued message to edit; shortened ones can't be edited here.
    var edit: ((QueuedMessage) -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Label("Queued (\(view.queue.count))", systemImage: "tray.full")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Spacer()
                if view.canInterruptAndSend {
                    Button("Send now", action: sendNow)
                        .font(.caption.weight(.semibold))
                        .buttonStyle(.bordered)
                        .controlSize(.mini)
                }
            }
            ForEach(view.queue) { message in
                HStack(alignment: .top, spacing: 8) {
                    Text(message.text)
                        .font(.callout)
                        .lineLimit(2)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                        .onTapGesture {
                            if !message.truncated { edit?(message) }
                        }
                        .accessibilityAddTraits(edit != nil && !message.truncated ? .isButton : [])
                        .accessibilityHint(edit != nil && !message.truncated ? "Edit the queued message" : "")
                    Button {
                        remove(message.index)
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Remove queued message")
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 8))
            }
        }
    }
}

/// Multi-line input with photo attachments. While the agent works it offers
/// Steer (send now) or Queue, and Stop when empty. Typing `/` at the start
/// lists the session's slash commands.
struct ComposerView: View {
    @Bindable var composer: ComposerModel
    let view: SessionView
    let commands: SlashCommandModel
    let serverName: String
    let send: (_ queue: Bool) -> Void
    let stop: () -> Void
    let choose: (SessionCommand) -> Void
    /// Loads the commands; `true` lists the provider's skills again.
    let loadCommands: (_ refresh: Bool) -> Void

    @State private var pickerItems: [PhotosPickerItem] = []
    @FocusState private var focused: Bool

    private var showsCommands: Bool {
        view.canSend && (commands.skillsOnly || SlashCommands.query(composer.text) != nil)
    }

    /// The draft runs one of difu's commands rather than messaging the agent.
    private var isCommandDraft: Bool {
        guard composer.attachments.isEmpty else { return false }
        if case .command = SlashCommands.submission(composer.text, commands: commands.commands) { return true }
        return false
    }

    var body: some View {
        VStack(spacing: 8) {
            if showsCommands {
                CommandMenu(
                    commands: commands, text: composer.text, serverName: serverName,
                    choose: { command in
                        choose(command)
                        focused = true
                    },
                    reload: { loadCommands(true) })
            }
            if !composer.attachments.isEmpty {
                attachmentStrip
            }
            HStack(alignment: .bottom, spacing: 8) {
                PhotosPicker(selection: $pickerItems, maxSelectionCount: 4, matching: .images) {
                    Image(systemName: "photo.on.rectangle.angled")
                        .font(.title3)
                        .frame(width: 32, height: 36)
                }
                .disabled(!view.canSend)
                .accessibilityLabel("Attach photos")

                TextField(placeholder, text: $composer.text, axis: .vertical)
                    .lineLimit(1...6)
                    .focused($focused)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 18))
                    .disabled(!view.canSend)

                actions
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .padding(.bottom, 6)
        .onChange(of: showsCommands) { _, shown in
            if shown { loadCommands(false) }
        }
        .onChange(of: pickerItems) { _, items in
            guard !items.isEmpty else { return }
            pickerItems = []
            Task {
                for item in items {
                    if let image = await PickedImage.load(item) {
                        composer.attachments.append(image)
                    }
                }
            }
        }
    }

    @ViewBuilder private var actions: some View {
        if composer.sending {
            ProgressView()
                .frame(width: 36, height: 36)
        } else if isCommandDraft {
            Button("Run") { send(false) }
                .buttonStyle(.borderedProminent)
                .foregroundStyle(Color.onAccent)
                .controlSize(.small)
                .padding(.bottom, 4)
                .accessibilityLabel("Run the command")
        } else if view.active, !view.preparing {
            if composer.hasContent {
                HStack(spacing: 6) {
                    Button("Queue") { send(true) }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    Button("Steer") { send(false) }
                        .buttonStyle(.borderedProminent)
                        .foregroundStyle(Color.onAccent)
                        .controlSize(.small)
                }
                .padding(.bottom, 4)
            } else {
                Button(action: stop) {
                    Image(systemName: "stop.circle.fill")
                        .font(.system(size: 30))
                        .foregroundStyle(.failure)
                }
                .accessibilityLabel("Stop the agent")
            }
        } else {
            Button {
                send(false)
            } label: {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 30))
            }
            .disabled(!composer.hasContent || !view.canSend)
            .accessibilityLabel("Send")
        }
    }

    private var attachmentStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(composer.attachments) { image in
                    Image(uiImage: image.thumbnail)
                        .resizable()
                        .scaledToFill()
                        .frame(width: 56, height: 56)
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                        .overlay(alignment: .topTrailing) {
                            Button {
                                composer.attachments.removeAll { $0.id == image.id }
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .symbolRenderingMode(.palette)
                                    .foregroundStyle(.white, .black.opacity(0.6))
                            }
                            .offset(x: 5, y: -5)
                            .accessibilityLabel("Remove photo")
                        }
                }
            }
            .padding(.top, 6)
            .padding(.horizontal, 2)
        }
    }

    private var placeholder: String {
        if !view.canSend { return "This session can't take messages right now" }
        if view.preparing { return "Message (queued until the agent is ready)" }
        return view.active ? "Steer or queue a message…" : "Message the agent, or / for commands…"
    }
}
