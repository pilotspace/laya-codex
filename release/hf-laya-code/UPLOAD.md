# Publishing laya-code to Hugging Face

Nothing here has been run. These are the commands to run by hand. They were checked against
`hf` CLI 1.8.0 (`hf repo create --help`, `hf upload --help`).

Set the target repo once. Replace `<hf-user-or-org>` with your account or organisation:

```sh
export HF_REPO=<hf-user-or-org>/laya-code
export CKPT=~/.cache/laya-codex/models/laya-code
export PKG=release/hf-laya-code        # run from the laya-codex repo root
```

## 1. Pre-flight

```sh
hf auth whoami                                        # must print the account that owns $HF_REPO
(cd "$CKPT" && shasum -a 256 -c "$OLDPWD/$PKG/MANIFEST.sha256")   # every line must say OK
```

Before uploading, fill in the `TODO` in `README.md` (per-repo training pair counts) and replace
`OWNER` in the laya-codex GitHub link.

## 2. Create the repo (private first)

```sh
hf repo create "$HF_REPO" --type model --private
```

## 3. Upload the checkpoint

Upload 7 files, the ones listed in `MANIFEST.sha256`. Leave out the local Python cache and the
internal training note `MODEL_CARD.md`; `README.md` from this package replaces it.

```sh
hf upload "$HF_REPO" "$CKPT" . \
  --exclude "__pycache__/*" --exclude "*.pyc" --exclude "MODEL_CARD.md" \
  --commit-message "laya-code: fine-tuned Laya code-relevance re-ranker"
```

## 4. Upload the model card, license and manifest

```sh
hf upload "$HF_REPO" "$PKG/README.md"        README.md       --commit-message "Model card"
hf upload "$HF_REPO" LICENSE                 LICENSE         --commit-message "Apache-2.0 license"
hf upload "$HF_REPO" "$PKG/NOTICE"           NOTICE          --commit-message "NOTICE"
hf upload "$HF_REPO" "$PKG/MANIFEST.sha256"  MANIFEST.sha256 --commit-message "sha256 manifest"
```

## 5. Verify, then publish

```sh
hf download "$HF_REPO" --dry-run                     # expect 11 files + the Hub's .gitattributes; no MODEL_CARD.md, no __pycache__
tmp=$(mktemp -d) && hf download "$HF_REPO" --local-dir "$tmp" \
  && (cd "$tmp" && shasum -a 256 -c MANIFEST.sha256) && rm -rf "$tmp"
```

Check the rendered card on the website: the license badge should say `apache-2.0`, and "Base
model" should link `convaiinnovations/laya`. Then make the repo public in the repo's Settings.

## 6. Point laya-codex at it

Once the repo is public, users can install the model with:

```sh
hf download "$HF_REPO" --local-dir ~/.cache/laya-codex/models/laya-code
```

## Rollback

If something is wrong, delete the files you uploaded
(`hf upload "$HF_REPO" <empty-dir> . --delete "*"`), or delete the repo on the website under
Settings → Delete. The local checkpoint is never modified by any of these commands.
