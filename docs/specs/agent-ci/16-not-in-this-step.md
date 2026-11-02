## 16. Not in this step

- **Deploys and releases.** They stay with the project's own pipeline or Retaskable's deploy features (staging deploy, promote, rollout). A project that wants deploys from its own CI while Retaskable runs the checks sets `ci` to `both`, or to `retaskable` and keeps its deploy workflow filtered to push events on the base (a deploy job should depend on its own checks, not on `retaskable/ci`, or be triggered by a push to the base that has the status `success`). Deploys already wait on the base's CI being green (the fix-main rule); that now reads `retaskable/ci` as it reads any check.
- **Scheduled and nightly runs**, matrix expansion beyond per-entry `needs`, artifacts and test reports uploaded anywhere, secrets for checks (a check that needs a secret reads it from the computer's own environment, which the owner set; the cloud never holds one).
- **Windows-only and macOS-only toolchains** are handled by tags, not by the agent installing anything.
- **Replacing the lane's checks.** The lane keeps running its checks before review.
- **Running CI for a project nobody has a computer for.** There is no hosted runner.
