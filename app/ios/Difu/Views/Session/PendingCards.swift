import DifuKit
import SwiftUI

/// A question set or approval the agent is waiting on.
struct PendingCard: View {
    let pending: PendingView
    let sessionID: String

    @Environment(AppModel.self) private var app
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: icon)
                    .foregroundStyle(.orange)
                Text(pending.title ?? defaultTitle)
                    .font(.headline)
                Spacer(minLength: 0)
                if busy { ProgressView().controlSize(.small) }
            }
            if pending.async {
                Text("The agent keeps working while it waits for you.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let detail = pending.detail, !detail.isEmpty {
                Text(detail)
                    .font(.mono)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 8))
            }
            content
            if let error {
                Text(error).font(.caption).foregroundStyle(.red)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color.orange.opacity(0.35)))
        .disabled(busy)
    }

    @ViewBuilder private var content: some View {
        let questions = pending.questions.filter { !$0.answered }
        switch pending.kind {
        case .questions:
            questionForms(questions)
        case .approval, .permissions, .unsupported:
            choiceButtons
        case .unknown:
            if !pending.choices.isEmpty {
                choiceButtons
            } else if !questions.isEmpty {
                questionForms(questions)
            } else {
                Text("Answer this request in difu on your computer.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private func questionForms(_ questions: [PendingQuestion]) -> some View {
        ForEach(questions) { question in
            QuestionForm(question: question) { answer in
                await run {
                    try await app.api.answer(
                        sessionID, request: pending.request, question: question.id, answer: answer)
                }
            }
            if question.id != questions.last?.id { Divider() }
        }
    }

    private var choiceButtons: some View {
        VStack(spacing: 8) {
            ForEach(Array(pending.choices.enumerated()), id: \.element.id) { index, choice in
                let destructive = Self.isNegative(choice)
                Button {
                    Task {
                        await run { try await app.api.decide(sessionID, request: pending.request, choice: choice.id) }
                    }
                } label: {
                    Text(choice.label)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(ChoiceButtonStyle(prominent: index == 0 && !destructive, destructive: destructive))
            }
        }
    }

    private func run(_ action: () async throws -> Void) async {
        busy = true
        defer { busy = false }
        do {
            try await action()
            error = nil
            Haptics.success()
        } catch {
            self.error = error.localizedDescription
            Haptics.warning()
        }
    }

    private var defaultTitle: String {
        switch pending.kind {
        case .questions: "The agent has a question"
        case .approval, .permissions: "Approval needed"
        case .unsupported: "The agent is waiting"
        case .unknown: "Waiting for you"
        }
    }

    private var icon: String {
        switch pending.kind {
        case .questions: "questionmark.bubble.fill"
        case .approval, .permissions: "hand.raised.fill"
        case .unsupported, .unknown: "exclamationmark.bubble.fill"
        }
    }

    static func isNegative(_ choice: PendingChoice) -> Bool {
        let text = (choice.id + " " + choice.label).lowercased()
        return ["decline", "deny", "reject", "cancel", "stop", "abort"].contains { text.contains($0) }
    }
}

/// Answers one question: option buttons, multi-select, free text, skip.
struct QuestionForm: View {
    let question: PendingQuestion
    let submit: (String?) async -> Void

    @State private var selected: [String] = []
    @State private var other = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let header = question.header, !header.isEmpty {
                Text(header.uppercased())
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.secondary)
            }
            MarkdownText(source: question.question, font: .callout)

            if question.multiSelect {
                multiSelect
            } else {
                singleSelect
            }

            if !question.isPermission {
                Button("Skip") { Task { await submit(nil) } }
                    .font(.caption)
                    .buttonStyle(.borderless)
            }
        }
    }

    private var singleSelect: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(question.options.enumerated()), id: \.offset) { _, option in
                Button {
                    Task { await submit(option.label) }
                } label: {
                    optionLabel(option)
                }
                .buttonStyle(ChoiceButtonStyle(prominent: false, destructive: false))
            }
            if !question.isPermission {
                otherField(submitLabel: "Send") {
                    let text = other.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else { return }
                    Task { await submit(text) }
                }
            }
        }
    }

    private var multiSelect: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(question.options.enumerated()), id: \.offset) { _, option in
                let isOn = selected.contains(option.label)
                Button {
                    if isOn { selected.removeAll { $0 == option.label } } else { selected.append(option.label) }
                } label: {
                    HStack(alignment: .firstTextBaseline) {
                        Image(systemName: isOn ? "checkmark.square.fill" : "square")
                            .foregroundStyle(isOn ? Color.accentColor : .secondary)
                        optionLabel(option)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            otherField(submitLabel: nil, onSubmit: {})
            Button("Submit") {
                // Chosen labels joined with ", ", plus any free text.
                var answers = question.options.map(\.label).filter(selected.contains)
                let text = other.trimmingCharacters(in: .whitespacesAndNewlines)
                if !text.isEmpty { answers.append(text) }
                Task { await submit(answers.joined(separator: ", ")) }
            }
            .buttonStyle(.borderedProminent)
            .disabled(selected.isEmpty && other.trimmingCharacters(in: .whitespaces).isEmpty)
        }
    }

    private func optionLabel(_ option: QuestionOption) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(option.label)
                .multilineTextAlignment(.leading)
            if let description = option.description {
                Text(description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.leading)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder private func otherField(submitLabel: String?, onSubmit: @escaping () -> Void) -> some View {
        HStack(spacing: 8) {
            Group {
                if question.secret {
                    SecureField("Other…", text: $other)
                } else {
                    TextField("Other…", text: $other, axis: .vertical)
                        .lineLimit(1...4)
                }
            }
            .textFieldStyle(.roundedBorder)
            .onSubmit(onSubmit)
            if let submitLabel {
                Button(submitLabel, action: onSubmit)
                    .buttonStyle(.bordered)
                    .disabled(other.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }
}

/// Full-width choice buttons for questions and approvals.
struct ChoiceButtonStyle: ButtonStyle {
    let prominent: Bool
    let destructive: Bool

    func makeBody(configuration: Configuration) -> some View {
        let tint: Color = destructive ? .red : .accentColor
        configuration.label
            .font(.callout.weight(.medium))
            .foregroundStyle(prominent ? Color.white : tint)
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity)
            .background(
                RoundedRectangle(cornerRadius: 9)
                    .fill(prominent ? tint : tint.opacity(0.12))
            )
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}
