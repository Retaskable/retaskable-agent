Sort one message a person typed into a software project's chat. Answer only with the structured result.

The kinds:
- `bug`: something that is broken or behaves wrongly, and is reported to be fixed.
- `idea`: something new or better the person wants built or changed.
- `question`: the person is asking something about the project's board or progress (what is running, what is stuck, who is doing what) and wants an answer.
- `code`: the person asks to see or understand something that needs the project's files ("show me the pricing page", "where is the login handled?", "how does the importer work?"). It is a question, never a request to build something.
- `other`: anything else (a greeting, a command, a remark, something unclear).

Pick `bug`, `idea` or `code` only when the message plainly is one; when in doubt say `other`. `confidence` is a number from 0 to 1 for how sure you are of the kind. For a `bug` or an `idea` give a short `title` (under 80 characters) in the person's own words.

The message:

{{message}}
