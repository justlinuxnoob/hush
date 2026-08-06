# Setup screenshots

Drop images in here named `step-1.png` … `step-6.png` and the setup wizard
shows them beside the matching instruction. Nothing else is needed — they are
picked up by filename, so adding one is not a code change.

Missing files are fine: the wizard simply shows no picture for that step.

| File | Page | What it should show |
|---|---|---|
| `step-1.png` | `console.cloud.google.com/projectcreate` | The **Project name** field and the **Create** button |
| `step-2.png` | Gmail API library page | The blue **Enable** button |
| `step-3.png` | `auth/branding` | The **App Information** step, or **Audience → External** selected |
| `step-4.png` | `auth/audience` | **Test users** with **Add users** visible |
| `step-5.png` | `auth/clients` | The **Application type → Desktop app** dropdown |
| `step-6.png` | after creating the client | **Only the Download JSON row.** |

## One rule

`step-6.png` must **not** include the Client ID or Client secret. Google shows
both on that dialog, and a real-looking credential in a public repository is
alarming whether or not it still works — people will report it, and anyone
copying the repo inherits the confusion. Crop to the `Download JSON` line and
nothing above it.

The same goes for the rest: crop out the account email in the top right and any
project name you would rather not publish.
