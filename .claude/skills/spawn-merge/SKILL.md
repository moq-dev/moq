---
name: spawn-merge
description: Decide and merge GitHub PRs in parallel.
---

Read the /merge, /takeover, and /close skills before starting.

The goal is to evaluate the open PRs in the repository and decide which ones to merge.
Each merge is performed in parallel by a sub-agent.

Start by listing all open PRs.
An argument can be used to filter the PRs in scope.

One at a time, for each PR, interactively prompt the user if we should /merge, skip, or /close.
Include a short summary and your recommendation.

If the user chooses to merge the PR, run the /merge command using a sub-agent.
There can be at most N concurrent merge operations, where N is half the number of physical CPU cores.
If ordering matters, then queue the PRs in order.

If the user chooses to close the PR, run the /close command using a sub-agent.

Keep going until all PRs have been decided then wait for all spawned sub-agents to finish.
Before finishing, refresh the open PR list and process any newly opened PRs that match the original scope.

Summarize the results when done.
Include all of the issues encountered and suggested follow-ups.
Repeat the process for any newly opened PRs that match the original scope.
