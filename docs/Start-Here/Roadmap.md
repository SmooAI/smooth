# Roadmap

#start-here

> [!info] Where Big Smooth is headed
> What we are building next to make Big Smooth a complete personal assistant for you and your family. These are directions, not dates. Each item becomes a pearl when work starts, and this page moves an item to the README's "What you get today" when it ships.

## Coming soon

| Surface                | Status                                                                                                                                                      |
| ---------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Big Smooth desktop app | In progress. Join the waitlist at [smoo.ai/big-smooth#coming-soon](https://smoo.ai/big-smooth#coming-soon).                                                 |
| iPhone and Android     | In progress. Same waitlist.                                                                                                                                 |
| Browser extension      | Today `th chrome` gives agents bookmarks and opt-in tab control ([[Engineering/Chrome-Extension]]). A chat-in-the-browser extension is next. Same waitlist. |

## Personal-assistant gaps

What people expect from a personal assistant that Big Smooth does not do yet, and what each one builds on.

| Gap                                 | What exists today                                                                                                                       | What's missing                                                               |
| ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| Proactive daily brief and check-ins | Calendar, reminders and `notify` tools; a scheduler that fires saved prompts (`th daemon schedule add`).                                | A ready-made morning summary of calendar, reminders and mail, on by default. |
| Routines set from chat              | Schedules can be added from the CLI (`th daemon schedule add`).                                                                         | "Every Friday remind me to…" said in chat creates and manages the schedule.  |
| Email triage for a personal inbox   | Org email tools on the Smoo AI platform.                                                                                                | A personal Gmail / iCloud connector, and triage on top of it.                |
| Voice in and out, and phone calls   | Calling exists on the Smoo AI platform.                                                                                                 | Talking to Big Smooth, and Big Smooth placing a call for you.                |
| Images in chat                      | Big Smooth can build and send files and HTML pages; the gateway hosts image models.                                                     | Understanding images you send, and generating images on request.             |
| Shared family memory and profiles   | Memory (`remember` / `recall`), cloud-backed when signed in; the multi-member design ([[Decisions/ADR-008-family-ai-multi-principal]]). | Per-member memory and profiles, and kid-safe guardrails (Smoo Jr).           |
| Cross-device continuity             | Memory syncs through the Smoo AI cloud when signed in ([[Decisions/ADR-009-scoped-cloud-memory-knowledge]]).                            | Conversation history that follows you across Mac, phone and web.             |
| Integrations people expect          | macOS Calendar, Reminders, Messages, Contacts, Location and weather.                                                                    | Google Calendar write, Spotify, smart home, maps and travel.                 |

Want one sooner? Open an issue on [SmooAI/smooth](https://github.com/SmooAI/smooth/issues).

## Related

- [[Start-Here/What-Is-Smooth]]
- [[Start-Here/Quickstart]]
- [[Architecture/Daemon-Direction]]
