import SwiftUI

// Above and in place of the composer in a Codex conversation, as Codex's own app has them (the phone
// version of the desktop's ui/codex/CodexLiveStrip.tsx and CodexRequestCard.tsx):
// - while a turn runs, its plan as one pill ("Step 2 / 5" beside a ring of how much is done) that lists
//   every step when tapped; the plan is not in the transcript;
// - while the agent waits on a question, a card in place of the composer: one question at a time (with
//   "1 / 2" and arrows when a request has several), options as numbered rows with their descriptions
//   and a Recommended badge (the first chosen from the start; a tap takes one and moves on), a row for
//   an answer of the reader's own when Codex welcomes one, Skip / Next, and X to close it — which stops
//   the turn, since Codex waits on the answer. A question that does not block the turn counts down to
//   when Codex goes on without an answer. Every question of the request is answered once the last is
//   done, one `answer` command each (an empty text for a skipped one; the host combines them). What was
//   chosen and typed is kept by the store, so it survives leaving the conversation, until the request is
//   over; the composer's own draft is untouched. The card never takes the keyboard by itself (the
//   reader taps into it); the composer gets it back once the card is answered or closed.

private func w(_ key: String, _ vars: [String: String] = [:]) -> String { phoneCodexText(key, vars) }
private typealias Ink = PhoneCodexInk

/// How much of the plan is done, as a ring.
private struct PlanRing: View {
    let done: Int
    let total: Int
    var body: some View {
        ZStack {
            Circle().stroke(Color.primary.opacity(0.15), lineWidth: 2)
            Circle().trim(from: 0, to: total > 0 ? CGFloat(done) / CGFloat(total) : 0)
                .stroke(Ink.accent, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .frame(width: 12, height: 12)
        .accessibilityHidden(true)
    }
}

/// The running turn's plan above the composer: "Step 2 / 5"; a tap lists the steps.
struct PhoneCodexPlanPill: View {
    let plan: [PhonePlanEntry]
    @PhoneViewState private var open = false
    var body: some View {
        let st = phonePlanStep(plan)
        let step = w("cdx_plan_step", ["s": "\(st.step)", "n": "\(st.total)"])
        Button { open = true } label: {
            HStack(spacing: 6) {
                PlanRing(done: st.done, total: st.total)
                Text(step).font(.caption.monospacedDigit())
            }
            .padding(.leading, 8).padding(.trailing, 10).frame(height: 26)
            .background(Ink.hover, in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain).foregroundStyle(open ? Ink.full : Ink.s60)
        .accessibilityLabel("\(w("cdx_plan")): \(step)")
        .accessibilityIdentifier("codex-plan")
        .popover(isPresented: $open, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(plan.enumerated()), id: \.offset) { _, e in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: e.status == "completed" ? "checkmark.circle" : e.status == "in_progress" ? "smallcircle.filled.circle" : "circle")
                            .font(.footnote)
                            .foregroundStyle(e.status == "completed" ? Ink.accent : e.status == "in_progress" ? Ink.full : Ink.s45)
                        Text(e.content).font(.footnote)
                            .foregroundStyle(e.status == "completed" ? Ink.s45 : e.status == "in_progress" ? Ink.full : Color.primary.opacity(0.65))
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .accessibilityElement(children: .combine)
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
            .frame(width: 280, alignment: .leading)
            .presentationCompactAdaptation(.popover)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("codex-plan-steps")
        }
    }
}

/// How long a chosen option shows as chosen before the card moves on.
private let flashTime = 0.18

/// The agent's question, in place of the composer.
struct PhoneCodexRequestCard: View {
    let req: PhoneCodexRequest
    @Binding var draft: PhoneCodexDraft
    let readonly: Bool
    /// Every question's answer, an empty one for a skipped question.
    let submit: ([(qid: String, text: String)]) -> Void
    /// Closed: the turn stops.
    let dismiss: () -> Void
    @PhoneViewState private var flash: Int?
    @FocusState private var typing: Bool

    private var n: Int { req.questions.count }
    private var q: PhoneQuestion { req.questions[min(draft.at, n - 1)] }
    private var pick: Int? { draft.picks[q.qid] }
    private var typed: String { draft.texts[q.qid] ?? "" }
    private var hasTyped: Bool { !typed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 8) {
                Text(q.prompt).font(.subheadline.weight(.medium)).foregroundStyle(Ink.full)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier("codex-request-prompt")
                if n > 1 { pager }
                if !readonly {
                    Button(action: close) { Image(systemName: "xmark").font(.footnote.weight(.semibold)).frame(width: 28, height: 28).contentShape(Rectangle()) }
                        .buttonStyle(.plain).foregroundStyle(Ink.s60)
                        .accessibilityLabel(w("cdx_q_close"))
                        .accessibilityIdentifier("codex-request-close")
                }
            }
            .padding(.leading, 16).padding(.trailing, 10).padding(.top, 12).padding(.bottom, 8)
            // A question that does not block the turn: Codex goes on by itself when nobody answers.
            if let expiresAt = req.questions.first(where: { $0.expiresAt != nil })?.expiresAt {
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    Text(w("cdx_q_expires", ["t": phoneCodexClock(expiresAt - context.date.timeIntervalSince1970 * 1000)]))
                        .font(.caption.monospacedDigit()).foregroundStyle(Ink.s45)
                        .accessibilityIdentifier("codex-request-expires")
                }
                .padding(.horizontal, 16).padding(.top, -4).padding(.bottom, 6)
            }
            VStack(alignment: .leading, spacing: 4) {
                if !q.options.isEmpty {
                    ForEach(Array(q.options.enumerated()), id: \.offset) { k, option in optionRow(k, option) }
                    if q.other { otherRow }
                } else {
                    freeField.padding(.horizontal, 8)
                }
            }
            .padding(.horizontal, 8)
            .id(q.qid)
            .transition(.opacity)
            if q.options.isEmpty || !q.other {
                HStack { Spacer(); action }.padding(.horizontal, 12).padding(.top, 6)
            }
        }
        .padding(.bottom, 10)
        .background(RoundedRectangle(cornerRadius: 20).fill(Color(white: 0.5).opacity(0.07)).background(.bar, in: RoundedRectangle(cornerRadius: 20)))
        .overlay(RoundedRectangle(cornerRadius: 20).strokeBorder(Ink.line))
        .shadow(color: .black.opacity(0.08), radius: 12, y: 4)
        .disabled(readonly)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("codex-request")
    }

    private var pager: some View {
        HStack(spacing: 0) {
            Button { go(draft.at - 1) } label: { Image(systemName: "chevron.left").frame(width: 24, height: 28).contentShape(Rectangle()) }
                .disabled(draft.at == 0)
                .accessibilityLabel(w("cdx_q_prev")).accessibilityIdentifier("codex-request-prev")
            Text(w("cdx_q_page", ["i": "\(min(draft.at, n - 1) + 1)", "n": "\(n)"])).monospacedDigit()
                .accessibilityIdentifier("codex-request-page")
            Button { go(draft.at + 1) } label: { Image(systemName: "chevron.right").frame(width: 24, height: 28).contentShape(Rectangle()) }
                .disabled(draft.at >= n - 1)
                .accessibilityLabel(w("cdx_q_next")).accessibilityIdentifier("codex-request-next")
        }
        .font(.caption).foregroundStyle(Ink.s60).buttonStyle(.plain)
    }

    private func optionRow(_ k: Int, _ option: String) -> some View {
        let split = phoneSplitRecommended(option)
        let on = pick == k
        let desc = q.descriptions.indices.contains(k) ? q.descriptions[k].trimmingCharacters(in: .whitespacesAndNewlines) : ""
        return Button { choose(k) } label: {
            HStack(spacing: 10) {
                ZStack {
                    Circle().fill(flash == k ? Ink.full : Color.primary.opacity(0.05))
                    Circle().strokeBorder(flash == k ? Ink.full : Ink.line)
                    if flash == k {
                        Circle().fill(Ink.canvas).frame(width: 8, height: 8)
                    } else {
                        Text("\(k + 1)").font(.caption.monospacedDigit()).foregroundStyle(Ink.s60)
                    }
                }
                .frame(width: 28, height: 28)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(split.label).font(.subheadline.weight(.medium)).foregroundStyle(Ink.full)
                        if split.recommended {
                            Text(w("cdx_q_recommended")).font(.caption).foregroundStyle(Ink.s60)
                                .padding(.horizontal, 6).padding(.vertical, 2)
                                .background(Color.primary.opacity(0.05), in: Capsule())
                                .accessibilityIdentifier("codex-option-recommended")
                        }
                    }
                    if !desc.isEmpty { Text(desc).font(.caption).foregroundStyle(Ink.s45).fixedSize(horizontal: false, vertical: true) }
                }
                .multilineTextAlignment(.leading)
                Spacer(minLength: 0)
                Image(systemName: "arrow.right").font(.footnote).foregroundStyle(Ink.s45).opacity(on ? 1 : 0).accessibilityHidden(true)
            }
            .padding(.horizontal, 8).padding(.vertical, 6)
            .background(on ? Ink.hover : Color.clear, in: RoundedRectangle(cornerRadius: 12))
            .contentShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
        .accessibilityIdentifier("codex-option-\(k + 1)")
    }

    /// The row for an answer of the reader's own (Codex welcomes one).
    private var otherRow: some View {
        HStack(spacing: 10) {
            Image(systemName: "pencil").font(.footnote).foregroundStyle(Ink.s60)
                .frame(width: 28, height: 28)
                .background(Color.primary.opacity(0.05), in: Circle())
                .overlay(Circle().strokeBorder(Ink.line))
            field(w("cdx_q_other_ph"), lines: 1...4)
            action
        }
        .padding(.horizontal, 8).padding(.vertical, 6)
        .background(typing || hasTyped ? Ink.hover : Color.clear, in: RoundedRectangle(cornerRadius: 16))
    }

    /// The answer itself, for a question without options.
    private var freeField: some View {
        field(w("cdx_q_type_here"), lines: 3...6)
            .padding(.horizontal, 10).padding(.vertical, 8)
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(typing ? Color.primary.opacity(0.28) : Ink.line))
    }

    @ViewBuilder private func field(_ placeholder: String, lines: ClosedRange<Int>) -> some View {
        let text = Binding(get: { typed }, set: { value in
            draft.texts[q.qid] = value
            // Typing an answer of one's own takes back the option chosen.
            if !q.options.isEmpty { draft.picks[q.qid] = nil }
        })
        Group {
            if q.secret {
                SecureField(w("cdx_q_secret_ph"), text: text)
            } else {
                TextField(placeholder, text: text, axis: .vertical).lineLimit(lines)
            }
        }
        .font(.subheadline)
        .focused($typing)
        .submitLabel(.next)
        .onSubmit(next)
        .onChange(of: typing) { _, on in if on && !q.options.isEmpty { draft.picks[q.qid] = nil } }
        .accessibilityIdentifier("codex-request-field")
    }

    /// Skip, or Next once something is typed.
    private var action: some View {
        Button(hasTyped ? w("cdx_q_next_btn") : w("cdx_q_skip"), action: next)
            .font(.footnote.weight(.medium))
            .foregroundStyle(hasTyped ? Ink.canvas : Ink.full)
            .padding(.horizontal, 12).frame(height: 28)
            .background(hasTyped ? Ink.full : Color.clear, in: Capsule())
            .overlay(Capsule().strokeBorder(hasTyped ? Color.clear : Color.primary.opacity(0.16)))
            .buttonStyle(.plain)
            .accessibilityIdentifier("codex-request-action")
    }

    private func choose(_ k: Int) {
        guard !readonly, flash == nil else { return }
        var x = draft
        x.picks[q.qid] = k
        draft = x
        flash = k
        typing = false
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(flashTime))
            flash = nil
            advance(x)
        }
    }

    /// On to the next question, or (on the last) the answers go.
    private func advance(_ x: PhoneCodexDraft) {
        typing = false
        if x.at < n - 1 {
            var next = x
            next.at += 1
            withAnimation(.easeOut(duration: 0.2)) { draft = next }
        } else {
            draft = x
            submit(x.answers(req.questions))
        }
    }

    private func go(_ i: Int) {
        typing = false
        withAnimation(.easeOut(duration: 0.2)) { draft.at = max(0, min(n - 1, i)) }
    }

    private func next() {
        var x = draft
        if hasTyped {
            x.picks[q.qid] = nil
        } else {
            // Skipped: no option, nothing typed.
            x.picks[q.qid] = nil
            x.texts[q.qid] = ""
        }
        advance(x)
    }

    private func close() {
        guard flash == nil else { return }
        dismiss()
    }
}
