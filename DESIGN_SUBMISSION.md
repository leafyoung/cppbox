# DESIGN_SUBMISSION.md — Submission & Marking System

Status: **design approved, not yet built**. Build starts on explicit go-ahead.

A system for students to submit C++ projects and for the teacher to collect,
organize, mark, and return feedback — built on top of CPPBox.

---

## 1. Topology

- **Students** and the **teacher** each run their own CPPBox instance locally
  (separate databases).
- **Outbound (student → teacher):** submission **zip** files.
- **Return (teacher → student):** **email** via the local mail client (`mailto:`).
- **Drive-sync** (e.g. Google Drive) is used only for the student's *working
  project* folder (`local_path`). It is **not** the feedback-delivery channel.

There is no live network connection between student and teacher machines. All
exchange is asynchronous (zips out, email back).

---

## 2. Core design decision: feedback is a *separate* file (no merge)

Marking is **not** in-place edits to the student's code. The teacher writes a
**separate markdown feedback file** with comments that *reference* the student's
files via `@file` mentions.

Because the teacher's artifact (`feedback.md`) and the student's artifacts
(`*.cpp`, `*.h`, …) are **disjoint files**, there is **no merge conflict** to
resolve. The feedback is purely additive. This eliminates the need for any
3-way merge, conflict markers, or base-tracking for merge purposes.

```
teacher writes:  feedback.md          (their artifact)
student writes:  src/*.cpp, *.h, …    (their artifact)
                 ───────────────
                 disjoint → nothing to merge
```

---

## 3. Data model

```
Class(id, name, course, cohort)

Student(id, class_id,
        serial   ← from import, NEVER auto-generated,
        name,
        email)

Assignment(id, class_id, name, slot, root_folder)

SubmissionKey(key        ← random auto-generated token,
              student_id, course, cohort, slot, assignment_id)

Submission(id, key, counter, project_id,
           commit_hash,   ← git commit of the submitted snapshot,
           zip_path,
           marked_at,     ← set when feedback is published (drives ✅ icon)
           feedback)      ← the markdown feedback content
```

Notes:
- `Student.serial` is **always provided in the import**; CPPBox never invents
  one. An import line without a valid serial is **flagged/rejected**, not
  silently numbered.
- `SubmissionKey.key` **is** auto-generated (random token). The "never
  auto-generate" rule applies to the **serial only**, not to keys.
- `Submission.counter` is the ever-increasing per-key submission count
  (1, 2, 3, …).

---

## 4. Naming & format templates

### Student sub-folder name
```
{serial:02d}-{name}
```
Serial zero-padded to width 2 (`f"{serial:02d}"`):
`01-Alice`, `02-Bob`, `09-Dave`, `10-Carol`, `23-Frank`.

### Submission zip name
```
{key}+{counter}.zip
```
e.g. `b2ed3fe4b54bd1f33903497fb8dfce03+1.zip`.

### Git commit message (one per submission)
```
Submission #{seq} {ISO-8601 timestamp, millisecond precision, UTC 'Z'}
```
Example:
```
Submission #1 2026-07-28T14:23:45.123Z
```
- `seq` — per-project submission sequence, **starting from 1** (a project's git
  log reads 1, 2, 3, … even if submitted to several assignments).
- timestamp — `datetime.now(timezone.utc)` formatted to milliseconds with a `Z`
  suffix: `YYYY-MM-DDTHH:MM:SS.mmmZ`.
- The commit **hash** is stored on the `Submission` row for precise lookup
  (the human-readable `#seq` is for the log; the hash is for reference).
- Purpose of the commit: **snapshot/history** and the stable target that
  `@file` references point to. (It is *not* a merge base — no merge is needed.)

### Student import format (one student per line)
```
serial,name,email
```
- first comma-separated field = **serial** (numeric, required)
- then `name`
- email either as a 3rd field or in `<…>` after the name

Examples:
```
1,Alice,alice@school.edu
2,Bob,bob@school.edu
10,Carol <carol@school.edu>
23,Frank
```
→ folders `01-Alice`, `02-Bob`, `10-Carol`, `23-Frank`.

---

## 5. Organized assignment folder structure

Each assignment has a **root folder**. Collected submission zips are unpacked
and organized as sub-folders named `{serial:02d}-{name}`:

```
Assignment1/                      ← assignment root folder
├── 01-Alice/                     ← one sub-folder = one CPPBox project
│   ├── src/main.cpp              ← Alice's submitted code (unpacked)
│   ├── include/util.h
│   ├── Makefile
│   └── feedback.md               ← 📝 teacher's marking lands here
├── 02-Bob/
│   └── …
├── 09-Dave/
│   └── …
└── 10-Carol/
    └── …
```

---

## 6. Student flow

1. Code in their project (Drive-synced `local_path`).
2. **📤 Submit** → paste their key →
   - build `<key>+<counter>.zip` (project files + `meta.json` carrying the key),
   - create a git commit `Submission #{seq} {timestamp}Z`, store its hash.

---

## 7. Teacher flow

1. **🎓 Admin → create class** (name, course, cohort).
2. **Import students** — `serial,name,email` per line; serial required, never
   auto-generated.
3. **Create assignment** (name, slot, root folder) → mint a **random key per
   student** → **✉ email keys** to students (mailto, local mail client).
4. **Collect submission zips → "unpack & organize":** read each zip's
   `meta.json` → `key → student(serial, name)` → unzip into `NN-Name/` under the
   assignment root.
5. **📂 Open assignment folder** (in-app action) → scan the root's immediate
   sub-directories → register each as a project (`local_path` = sub-folder,
   name = sub-folder name). **Idempotent** — re-opening skips folders already
   registered.
6. **Open a student project → marking editor:**
   - left pane: **read-only** code viewer (the student's submitted files),
   - right pane: **feedback.md** editor with **`@`-file autocomplete**,
   - **Publish** → save feedback, set `marked_at` (grid cell → ✅).
7. **✉ Email feedback** → `mailto:` to the student with the feedback markdown
   as the email body.
8. **Grid overview** — students × assignments matrix with status icons.

---

## 8. The `@`-file reference & autocomplete

In the feedback editor, typing `@` pops an autocomplete listing the **student's
submitted files**; selecting one inserts `@path/to/file`. Optional `@file:LINE`
acts as a textual pointer to a specific line.

```markdown
# Feedback — Assignment 1

## @src/main.cpp
- L12: prefer a range-for loop here.
- @src/main.cpp:25 — missing null check before deref.

## @include/util.h
- Add `#pragma once`.

## General
- Good separation of concerns. Watch the magic numbers in @src/math.cpp.
```

The `@file` references make feedback navigable. In the student's view they can
render as clickable jumps to the file/line (polish phase). The autocomplete
reuses the same CodeMirror hint mechanism already used for clangd, with a static
file list as the source.

---

## 9. The admin grid

Admin → pick a class → a students × assignments matrix:

```
FN6805 AY26                          [+ Import students] [+ New assignment]
┌───────────────┬─────────────────┬─────────────────┬─────────────────┐
│ Student       │ Assignment 1    │ Assignment 2    │ Assignment 3    │
├───────────────┼─────────────────┼─────────────────┼─────────────────┤
│ Alice         │ ✅ marked  #2    │ 📩 new     #1   │ —               │
│ Bob           │ 📩 new     #1    │ —               │ —               │
│ Carol         │ —               │ —               │ —               │
└───────────────┴─────────────────┴─────────────────┴─────────────────┘
```

Cell states:
| Icon | Meaning | Click does |
|---|---|---|
| `—` | not submitted | nothing |
| 📩 `#N` | submitted, **unmarked** | open marking editor |
| ✅ `#N` | **marked** | reopen / edit marks |

`#N` = the student's latest attempt number.

---

## 10. Feedback delivery — email

Feedback is delivered by **email** using the local mail client (`mailto:`), the
same mechanism used to email submission keys.

- "Email feedback" opens `mailto:student@email?subject=…&body=<feedback markdown>`.
- The `feedback.md` file is also kept as the teacher's **local record**
  (re-sendable).

**Caveat:** `mailto:` cannot attach files and has URL-length limits. For very
long feedback, attach `feedback.md` manually.

---

## 11. What reuses existing machinery

- **`local_path` projects** — "Open assignment folder" registers each sub-folder
  as a `local_path` project; file tree, editor, run, and Drive-sync already work
  off `local_path`. No new storage path.
- **CodeMirror hint addon** — the `@`-file autocomplete reuses the same mechanism
  as clangd completion.
- **`mailto:`** — already designed for emailing submission keys; reused for
  feedback.
- **git** — already in the stack (host + sandbox); used for the per-submission
  commit.

## 12. What was deliberately dropped

Earlier a git 3-way merge design was explored (merge teacher's in-place edits
with the student's later work). It was **dropped** in favour of the separate
feedback file, which makes merge unnecessary:

| Earlier idea | Now |
|---|---|
| `git merge-file` 3-way merge | ❌ not needed |
| conflict markers / resolver UI | ❌ not needed |
| base-stashing for merge | ❌ not needed |
| submission git commit | ✅ kept — for snapshot/history + `@file` target |

---

## 13. Caveats & assumptions

- `mailto:` can't attach files / has URL-length limits → long feedback: attach
  `feedback.md` manually.
- "Open assignment folder" reads **local filesystem paths** (local-tool
  assumption — consistent with `local_path`; the teacher has full local access).
- Student serial must be numeric (folder naming uses `{:02d}`).

---

## 14. Build phases

1. **Submission git commit** — `Submission #{seq} {ts}Z` + store hash.
   (Fully specified, standalone.)
2. **Class / student import** (serial from import, never auto-gen) +
   **assignment** + **key minting** + **email keys**.
3. **Unpack & organize** zips → `NN-Name` folders.
4. **Open assignment folder** → sub-folders as `local_path` projects.
5. **Marking editor** — read-only code viewer + `feedback.md` + `@` autocomplete
   + Publish.
6. **Email feedback** (mailto).
7. **Admin grid** overview (students × assignments).

---

## 15. Decisions log

| Topic | Decision |
|---|---|
| Feedback location | Separate markdown file, **not** in-place edits |
| Merge strategy | None needed (disjoint files) |
| Student serial | From import, **never auto-generated**, `{:02d}` |
| Submission keys | Random auto-generated tokens (kept) |
| Submission identifier for auth | The random **key** (student pastes it) |
| Folder naming | `{serial:02d}-{name}` |
| Commit message | `Submission #{seq} {ISO-ms}Z`, seq per-project from 1 |
| Commit timestamp | UTC, millisecond precision, `Z` suffix |
| Workspace launch | **In-app "Open assignment folder"** action (not startup config) |
| Feedback delivery | **Email** via local mail client (mailto) |
| Marking tool | **In-app** marking editor (entered via the grid / opened project) |
