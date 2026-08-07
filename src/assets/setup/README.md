# Setup screenshots

Drop images in here named `step-1.png` … `step-6.png` and the setup wizard
shows them beside the matching instruction. Nothing else is needed — they are
picked up by filename, so adding one is not a code change.

Missing files are fine: the wizard simply shows no picture for that step.

**These are filled in.** `step-1` through `step-5c` are real captures of
Google's console. If Google redesigns a page, replacing the file is the whole
fix — no code changes.

A step can have several pictures. `step-5.png`, `step-5b.png` and `step-5c.png`
all belong to step 5 and show in that order — the last step is three separate
actions, so one image cannot carry it.

| File | Page | What it should show |
|---|---|---|
| `step-1.png` | `console.cloud.google.com/projectcreate` | The **Project name** field and the **Create** button |
| `step-2.png` | Gmail API library page | The blue **Enable** button |
| `step-3.png` | `auth/branding` | **Audience → External** selected — the choice people get wrong |
| `step-3b.png` | `auth/branding` | Optional: the **App Information** step above it |
| `step-4.png` | `auth/audience` | **Test users** with **Add users** visible |
| `step-5.png` | `auth/clients` | The **Create client** button |
| `step-5b.png` | `auth/clients` | The **Application type** dropdown with **Desktop app** in it |
| `step-5c.png` | after creating | **Only the Download JSON row.** See below. |

## One rule

`step-6.png` must **not** include the Client ID or Client secret. Google shows
both on that dialog, and a real-looking credential in a public repository is
alarming whether or not it still works — people will report it, and anyone
copying the repo inherits the confusion. Crop to the `Download JSON` line and
nothing above it.

The same goes for the rest: crop out the account email in the top right and any
project name you would rather not publish.
