// semantic-release plugin: exposes the computed next version as the
// `version` step output in GitHub Actions (also in --dry-run mode).
import { appendFileSync } from "node:fs";

export function verifyRelease(_pluginConfig, { nextRelease, logger }) {
  logger.log("Next release version: %s", nextRelease.version);
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `version=${nextRelease.version}\n`);
  }
}
