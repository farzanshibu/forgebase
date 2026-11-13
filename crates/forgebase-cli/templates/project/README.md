# {{NAME}}

A ForgeBase app project: the server configuration lives in [`{{DIR}}/`]({{DIR}})
as versionable files (collections, functions, queues, buckets), applied with
the ForgeBase CLI. See ForgeBase's `docs/project-as-code.md` for the layout.

```sh
cp .env.example .env              # then set FORGEBASE_URL and FORGEBASE_TOKEN
forgebase project validate        # check the files (no server needed)
forgebase project plan            # what would change on the server (exit 2: changes)
forgebase deploy                  # build functions, then apply the plan
forgebase project drift           # report changes made outside this project
forgebase project pull --force    # write the server's state back into the files
```

`{{DIR}}/seed/*.jsonl` holds fixtures (one JSON record per line) for a local
development server; they are never applied to a real server.

CI: `.github/workflows/forgebase.yml` runs `forgebase project plan` on pull
requests and `forgebase deploy` on `main`. Set the repository secrets
`FORGEBASE_URL` and `FORGEBASE_TOKEN` (an API key of a service account with
the `admin` role){{SECRETS}}.
