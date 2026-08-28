# ExecPlans

Use an ExecPlan for a complex feature or a significant refactor. Follow `.agents/PLANS.md` from design through implementation.

Use `.agents/` as the only repository namespace for planning and design artifacts that agents own. Do not create `.agent/`.

Store each ExecPlan in `.agents/plans/`.

Keep `.agents/PLANS.md` as the standard for ExecPlans. Do not store individual ExecPlans next to `.agents/PLANS.md`.

Store design notes for LLMs or agents in `.agents/docs/`. These notes can include agent context, publication runbooks, parity notes, and similar internal information. Do not publish these notes as product documentation.

Reserve `docs/` for future documentation for human readers. Do not store ExecPlans, agent runbooks, or LLM-only context in `docs/`.

# Git / version control

Commit directly to the current branch. This rule also applies when the current branch is `master`.

Do not create a branch, change branches, rebase, or open a pull request unless the user gives an explicit instruction.

Do not run `git checkout -b`.

The instruction "commit" means that you must commit on the current branch. It does not mean that you must create a branch first. This rule overrides other default branch procedures.

Stage and commit only the files that you changed. Do not run `git add -A`. Do not include unrelated working-tree changes in the commit.

Push to origin/master unless instructed otherwise.

# Expectations

Continue when there is a clear next step toward the goal. This rule applies inside and outside an ExecPlan. Do not stop to ask for approval.

If you made material progress, first make a multiline checkpoint commit. Explain the work to this point. Give detailed reasons for the changes. The diff shows the changes themselves.

