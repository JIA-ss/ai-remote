# Public launch discovery record

Canonical project: [JIA-ss/ai-remote](https://github.com/JIA-ss/ai-remote). Scope: the GitHub repository
and its English/Chinese README, not a separately hosted website. Audience: developers connecting
MCP agents to their own paired Windows/Mac computers, and people supervising that work.

## What changed

- A clear project H1, consistent official name, public repository description and relevant topics.
- New light/dark bilingual cover art using the existing blue agent / amber owner identity.
- Source install instructions, client-specific MCP setup, permissions and common questions.
- Platform and validation links close to the claims they qualify; iPhone source remains experimental.
- Public builds use included, licensed dependency snapshots rather than a private Git fork.
- Contribution instructions reflect the actual workflow; no nonexistent automatic CLA is promised.
- GitHub private vulnerability reporting is enabled and linked by SECURITY.md.

The README answers what is built, which platforms have evidence, how to connect Codex/Claude/Cursor/
VS Code, whether an iPhone is a controller or a host, and which capabilities remain planned. There
is no signed release, App Store listing or universal accessibility-tree claim.

## Measurement

[Launch baseline](launch-baseline.json) freezes four brand/non-brand queries and preserves returned
source titles/URLs, options and capture time. The surface is the web search tool; engine and region
are not exposed. Returned order is not reported as ranking. The first sample returned no official
project URL for any of the four queries. This newly public repository's absence in that sample does
not establish absence from an index.

Stages are recorded independently:

| Stage | Evidence required |
| :-- | :-- |
| Technical delivery | Anonymous HTTP response, actual public content and source revision |
| Real crawler access | Server/crawler logs; GitHub does not expose these to this project |
| Indexing | Search-platform evidence; not inferred from HTTP 200 |
| Query discovery | Same frozen query strings/options and returned official source URL |
| Answer citation / recommendation | A captured answer and its actual project citation/recommendation |
| Adoption | Independent install/use evidence or project-owned analytics |

GitHub controls the domain's robots policy, canonical and sitemaps. This repository does not claim
control over them or add a project-path robots file. No directory submission, IndexNow receipt or
search-console ownership is claimed. Ordinary factual text and useful examples are the changes;
AI-specific instructions or markup are not ranking evidence.

## Follow-up method

After publication, check the anonymous repository page, both README versions and source revision;
repeat the exact launch-v1 queries on the same surface and preserve all outcomes, including negatives.
Give crawling/indexing time: same-day movement cannot prove that these edits caused an improvement.
A future measurement should keep these queries and record the elapsed interval. No ongoing schedule
has been created for this one-pass task.

References: [Google's guidance for AI features](https://developers.google.com/search/docs/appearance/ai-features),
[GitHub Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions).
The use of standard GitHub-hosted runners for public repos is free; larger runners and storage have
separate billing rules. The workflows use standard runners, dependency caches and one-day artifacts.

## Published result — 2026-10-10

The implementation/homepage merged as `a169db85e35e23f1c94e3bf3d768c335dde801bc`.
[Independent post-publication evidence](launch-post-publication.json) confirms anonymous main API,
fixed-revision English/Chinese content, link targets, repository description/topics, and public iOS
CI success. [HTTP audit](launch-http-audit.json) records a 200 repository page, actual H1 and
description; its inferred sitemap request failed. GitHub controls that domain, so this remains an
unavailable sitemap observation rather than a project-owned sitemap repair.

The [earlier independent sample](launch-independent.json) and post-publication sample both used
individual calls for each frozen query. Later source counts were 12 / 40 / 38 / 24, with no official
project URL returned. No immediate discovery increase was observed. Indexing, real crawler access,
answer citation, recommendation and adoption remain unmeasured.

A source discrepancy is retained: real-time anonymous repository HTML contains the merged revision
and new content, while the web tool's open result showed earlier text without a source revision.
No cache or indexing cause is asserted from that difference.

本轮完成公开入口、中英文主页、接入示例、平台能力边界与独立复核。四条固定查询暂未返回官方
仓库，不能把它解释为未索引，也不能宣称曝光提升。后续沿用同一查询集、记录时间间隔和真实
来源；引用、推荐与采用需要各自的实际证据。
