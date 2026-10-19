---
name: branch
description: Create a git branch following the convention authorname/type/descriptive-name. Use when user wants to create a new branch.
user-invocable: true
---

# Create Branch

Create a git branch following the convention: `authorname/type/descriptive-name`

## Usage

`/branch [TYPE] description`

## Workflow

1. Get author name from `git config user.name` and convert to kebab-case
2. Determine branch type from input or default to `feature`
3. Convert description to kebab-case (lowercase, spaces to hyphens, remove special chars, max 50 chars)
4. Create and checkout: `git checkout -b {authorname}/{type}/{descriptive-name}`

## Branch Types

| Type | Use For |
|------|---------|
| `feature` | New features (default) |
| `feat` | Alias for feature |
| `fix` | Bug fixes |
| `hotfix` | Critical production fixes |
| `refactor` | Code refactoring |
| `perf` | Performance improvements |
| `chore` | Maintenance tasks |
| `docs` | Documentation updates |

## Examples

- `/branch implement semantic search` → `osama-haikal/feature/implement-semantic-search`
- `/branch fix truncate long breadcrumb titles` → `osama-haikal/fix/truncate-long-breadcrumb-titles`
- `/branch hotfix exclude internal items on RAG` → `osama-haikal/hotfix/exclude-internal-items-on-rag`

## Rules

- Always use kebab-case for all parts
- Always get author name from git config
- Default to `feature` type if not specified
- Never create branch if it already exists (suggest alternative)
