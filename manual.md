# Manual: editing the school knowledge base

This guide explains how to change what the Nobles International School assistant
knows, how to add new topics, and how to check the result. No Rust or TypeScript
changes are needed for normal content updates.

---

## 1. How the knowledge file works

- The assistant's only school source is `school_knowledge.md` in the project
  root.
- The file is read once when the server starts. Editing it has no effect until
  the server is restarted.
- The complete file is inserted into the AI system prompt on every request,
  inside `--- BEGIN school_knowledge.md ---` and
  `--- END school_knowledge.md ---`.
- The system prompt tells the model to use only this file, to say it does not
  know when an answer is missing, and to direct parents to the main office.
- The assistant also receives the full saved conversation for that browser, so
  long chats plus a long knowledge file both consume model context.

Relevant code: `load_school_knowledge` and `build_system_prompt` in
`src/main.rs`. The path can be changed with `SCHOOL_KNOWLEDGE_PATH`.

### The school name

The server takes the school name from the first `#` heading in the file. If the
`SCHOOL_NAME` environment variable is set, it overrides that heading. The name is
used in the AI system prompt and the startup log; the browser title and header
are separate HTML in `static/index.html`.

Keep these three in sync when the name changes:

1. The first `#` heading in `school_knowledge.md`.
2. `SCHOOL_NAME` in `.env` (quote values containing spaces:
   `SCHOOL_NAME="Nobles International School"`).
3. The title, meta description, header, footer, and welcome text in
   `static/index.html`.

---

## 2. Golden rules for good answers

1. **Only add confirmed facts.** Use the school office, official letters,
   newsletters, or the school website. Never fill gaps with plausible details.
   A wrong date or fee is worse than "I don't know".
2. **Write for the parent, not for a search engine.** Short sentences, plain
   words, and the exact wording parents use ("term dates", "holidays", "bus",
   "school fees").
3. **Use absolute dates.** Write the day, month, and year in full, not "next
   term" or "in January". Relative wording goes stale silently.
4. **Say when something is not published.** An explicit "the website does not
   publish the fee schedule; call the school" stops the model from guessing.
5. **One topic per section, one fact per bullet.** Long paragraphs are harder
   for the model to quote accurately.
6. **Keep contact details identical everywhere.** If the phone number appears in
   three places, all three must match.
7. **Do not duplicate policies.** If two sections disagree, the model may pick
   either one.
8. **Do not put secrets or private data here.** The file is sent to Groq and
   OpenRouter as part of the prompt. No passwords, staff home numbers, medical
   details, or confidential parent information.
9. **Do not write instructions to the AI.** This file is data. Text such as
   "ignore previous instructions" or "always answer X" is risky and belongs in
   the system prompt, not the knowledge base.
10. **Keep it focused.** Every line is sent on every request. Aim for a few
    thousand words at most; trim old events and repeated explanations instead of
    letting the file grow forever.

---

## 3. File structure

Use this shape:

```markdown
# School Name

> One or two lines describing the file and how it is maintained.

## About the school
...

## Contact and location
...

## Admissions
...

## Academic programme
...

## School calendar
...

## Sports and clubs
...

## Scholarships and awards
...

## Common questions
...

## Not published on the school website
...

## Source
Last verified: <date> against <URLs>.
```

Rules for structure:

- Exactly one `#` heading, at the top. It is the school name.
- Use `##` for main topics and `###` for sub-topics.
- Keep the "Common questions" section near the end; it is the easiest place to
  improve answer quality.
- Keep the "Not published" section accurate. It is a guardrail, not a to-do
  list.
- Update the "Source" date whenever you change a fact.

---

## 4. Add new information in three steps

### Step 1: Edit the file

Open `school_knowledge.md` and add a section or a question. Save the file as
UTF-8.

### Step 2: Restart the server

```bash
npm run dev      # development server, rebuilds if needed
# or
npm start        # release binary
```

The file is read only at startup. A browser refresh alone is not enough.

### Step 3: Ask the exact question

Open <http://localhost:3000>, start a new conversation, and ask the question the
way a parent would. Check that the answer:

- matches the file,
- does not add details that are not in the file,
- points to the school office when the answer is missing.

You can also test the API directly:

```bash
curl -s http://localhost:3000/chat \
  -H "Content-Type: application/json" \
  -d '{"user_id":"11111111-1111-4111-8111-111111111111","message":"What are the school hours?"}'
```

Use a fresh UUID for a clean conversation, or reuse one to test follow-up
questions.

---

## 5. Templates you can copy

### A new topic section

```markdown
## After-school care

- **Hours:** <confirmed hours>
- **Location:** <confirmed location>
- **Eligibility:** <classes or ages>
- **Booking:** <how a parent books>
- **Contact:** <phone or office>

<Any extra confirmed detail in one or two sentences.>
```

### A new common question

```markdown
**Does the school provide after-school care?**
Yes. <confirmed answer with times and how to book>. If you need a place, call
<number>.
```

### A new procedure

```markdown
## Reporting an absence

1. Call the school office on <number> before <time>.
2. Give the student's name, class, and reason for absence.
3. <Any follow-up step the school requires.>
```

### Updating a date-sensitive item

```markdown
## Term dates 2027–2028

- **First term:** <start date> to <end date>
- **Second term:** <start date> to <end date>
- **Third term:** <start date> to <end date>

These dates were confirmed by the school on <date>. Parents should check the
school calendar for mid-term breaks and public holidays.
```

Do not leave angle brackets (`<...>`) in the final file. They are placeholders.

---

## 6. Worked examples

### Example 1: adding a verified term calendar

Before, the file only describes the three-term pattern. When the school issues
the calendar:

1. Add a `## Term dates 2027–2028` section after `## School calendar`.
2. List the three terms with exact dates.
3. Add one sentence for mid-term breaks and public holidays.
4. Remove any older term dates so there is only one current calendar.
5. Update the "Last verified" date.
6. Restart and ask: "When does the first term start?"

### Example 2: correcting a phone number

1. Search the file for the old number.
2. Replace every occurrence, including the "Common questions" section.
3. Update the "Last verified" date.
4. Restart and ask: "What is the school's phone number?"

### Example 3: adding a policy

1. Choose the right section, or create a new `##` section.
2. Write the policy as short bullets: who it applies to, what the rule is, what
   the parent should do, and who to contact.
3. Add a matching question under "Common questions".
4. Restart and test one normal case and one edge case.

### Example 4: adding an event

Events go stale quickly. Include the year in the heading and delete the event
after it has passed:

```markdown
## Events

- **Speech and Prize-Giving Day:** <date>, <time>, <venue>. <Who attends and how
  to get tickets.>
```

---

## 7. What to add first

If the file is thin, add information in this order. These are the questions
parents ask most often:

1. Exact term dates and holiday dates for the current session.
2. School hours, drop-off and pickup times.
3. Fees, what they cover, payment methods, and deadlines.
4. Admission steps, requirements, and deadlines.
5. Uniform requirements and where to buy them.
6. Attendance and absence reporting.
7. Transport routes and pickup points.
8. Contact details for the office, accounts, and sick bay.
9. Examination and assessment rules.
10. Behaviour, safeguarding, and visitor rules.

For each item, record only what the school has confirmed. If a fact is not
available yet, leave it in the "Not published" section with a note to call the
office.

---

## 8. Keeping the file accurate

### At the start of each term

- Update term dates and remove the previous session's dates.
- Update fees and payment deadlines.
- Remove events that have passed.
- Re-check phone numbers and the school address.

### Whenever the school website changes

- Compare the website's contact, admission, academic, and scholarship pages
  with the file.
- Copy only facts, not marketing claims that could change.
- Update the "Last verified" date and list the pages checked.

### Monthly spot check

Ask the assistant five questions from the "Common questions" section. If any
answer is wrong, fix the file rather than the model. The file is the source of
truth.

---

## 9. Common mistakes

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| The answer still uses old information | Server not restarted | Restart the server |
| The assistant says it does not know | The fact is missing or phrased vaguely | Add a clear bullet and a common question |
| The assistant invents a date or fee | The file is silent or contradictory | Add the confirmed fact, or state that it is not published |
| The school name is wrong | `SCHOOL_NAME` overrides the H1 | Update `.env` and the HTML title/header |
| The answer is cut off or slow | The file or chat history is very long | Trim repeated content and start a fresh conversation |
| The server exits at startup | File missing, empty, or unreadable | Check the path and permissions; `SCHOOL_KNOWLEDGE_PATH` must point to a readable file |
| A number or address is inconsistent | Duplicated in several sections | Search the file and update every occurrence |
| Angle brackets appear in an answer | Placeholder left in the file | Replace `<...>` with the confirmed value |

---

## 10. Quick checklist

- [ ] Fact confirmed by the school, not guessed
- [ ] Added under the right `##` section
- [ ] Absolute date with year, or explicitly "not published"
- [ ] Contact details match the rest of the file
- [ ] No duplicate or contradictory statement
- [ ] "Last verified" date updated
- [ ] No placeholders left
- [ ] Server restarted
- [ ] Test question answered correctly
- [ ] No secrets or private data added

---

## 11. Where to change code instead

Knowledge updates never require code changes. Use the codebase only when you
need a new capability, for example:

- a new HTTP endpoint: `src/main.rs`
- a different AI provider or model order: `.env`, `src/main.rs`
- a new Neon service call: `src/neon.rs`
- a change to the chat interface: `static/index.html`
- build and deployment commands: `README.md`

For those changes, follow the development checks in `README.md`.
