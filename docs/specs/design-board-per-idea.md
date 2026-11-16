# Design board per idea — Spec

Status: copy of section 14 of Retaskable.com's [design-view.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/design-view.md) ([design-view/14](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/design-view/14-no-chat-an-idea-gets-its-own-design-board.md); backlog id `design-board-per-idea`, 2026-11-16), trimmed to what the agent does. Retaskable.com stays the source: change it there first, then here. What was left out is in [TRIMMED.md](TRIMMED.md).

Theme: Design

Kind: change

## 1. What it is

There is no design chat. An idea whose change people will see gets **a whiteboard of its own, already holding the designs**, and the idea waits for the person to review them.

- **[Decided]** An idea with a user-experience change gets a new whiteboard added to it, on its own, holding the designs for review. The idea then needs the person: it stops and asks for their review before anything is built.
- **[Proposal]** **What counts.** The idea is a **feature** that adds or changes something a person sees (`needsDesignOf`, `shared/planProgressLines.ts`; `needsDesign` on the draft and on the store's idea row). A fix, a tweak, wording, colour-only and behaviour-only changes and an idea whose title starts "Design:" get no board. Decided in plain code from the drafter's structured result, never by asking the model beyond it. `needsDesign` is written only for a plan not written before, so a plan sent back is never moved again.
- **[Proposal]** **One board per idea**, a separate document from the project's shared Whiteboard, keyed by the idea's id (`ideaBoardId(ideaId)` is `idea-<idea id>`, `shared/boardDoc.ts`), made once per idea when the draft is written with `needsDesign`, and dropped with the idea.
- **[Proposal]** **Who draws it.** The agent that wrote the plan, as part of the same draft: a read-only session with no tools (the role and budget the draft has) is given the idea, the drawing table and the pages' parts and answers a layout (**Now**, up to three variations, each a desktop and a phone page, the changed region marked, a dashed **New** piece where the library has nothing), checked in plain code by `checkLayout`. The layout is written to the idea's board as the chat's (`who: "chat"`), so a person's edits are never overwritten.
- **[Proposal]** The plan ends at **Needs a design review** instead of "Plan ready". `draft_approve` refuses a plan with `needsDesign` ("This plan needs a design review first"). `draft_skip_design` drops `needsDesign` and records "Design skipped". **Approve design** sends the same plan back (`draft_revise`) with the board's chosen design written into the request; no second idea is filed.
- **[Proposal]** No release flag in the agent: it ships On.

## 2. The store's idea row

The idea row (`StoreIdea`, `shared/storeIdeas.ts`) carries `needsDesign: boolean` (TS #3875), true once the idea is a feature that changes what people see. A row from a store that doesn't send it reads as false; the agent writes it back unchanged when it serialises a row.

## 3. How the agent draws it

When the drafter writes a plan with `needsDesign`, `drawDesignBoard` reads the idea's board (the plan's store id is the idea id: `ideaBoardId(inStore.id ?? id)`), asks a read-only session with no tools for a layout, checks it with `checkLayout` against the drawing table and the pieces on the board, places it with `placeLayout` and writes the new pieces with their `chat` marks to the board's socket (a `board_ticket`, then `/board?ticket=&board=`), one `add` per message, at most 20 a second, waiting for the hub's acks.

- It never changes a piece a person touched; a redraw's `replaces` takes away only the chat's own untouched ones, and the new design is placed beside what stays.
- When it cannot draw (the project's backlog not open, signed out, paused, over budget, a layout that fails its check, the board not answering) the plan still waits for the design review and keeps the reason as `designNote`, cleared once a drawing lands.
- `board_design_write { draftId, note? }` runs the same drawing again for a plan that is awaiting approval with `needsDesign`; **Change it…** sends a `note`, and the board is redrawn with it, never beside the old designs (they stay as variations the person can keep or delete).
- The board holds drawn pieces and stickies only, never a transcript. The audit log keeps the plan's id and the note's length only.

## 4. Build slices on the agent

1. The `needsDesign` idea field on the Rust idea record, its serialisation and tests (this copy's first piece).
2. The agent draws it: the prompt, the layout schema and its check, the board socket write, `board_design_write`.
3. `draft_approve` refusal, `draft_skip_design`, `draft_revise` for **Approve design**.

## 5. Acceptance (the agent's part)

- A feature idea that changes a screen ends at **Needs a design review** with its own board of designs already on it. A fix, a tweak and a behaviour-only idea get no board. An idea gets its board once.
- A person's moved or edited piece on the board is never overwritten by a redraw.
- No command accepts `design_chat`.
