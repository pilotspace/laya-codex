# Homebrew formula for laya-codex. Published as Formula/laya.rb in pilotspace/homebrew-tap:
#   brew install pilotspace/tap/laya
# Regenerate for a new release with: scripts/update-formula.sh <version>
class Laya < Formula
  desc "Ranked code retrieval for Claude Code (tree-sitter + Moon + Laya re-ranker)"
  homepage "https://github.com/pilotspace/laya-codex"
  # laya is Apache-2.0; the bundled Moon sidecar ships with a GPL-3.0 LICENSE text.
  license all_of: ["Apache-2.0", "GPL-3.0-only"]

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    on_arm do
      url "https://github.com/pilotspace/laya-codex/releases/download/v0.1.2/laya-v0.1.2-aarch64-apple-darwin.tar.gz"
      sha256 "7a24a18cc1b637fb9c2d02340ed2e44cfed1730b919d69ea256e081cae0cf4c0"

      resource "moon" do
        url "https://github.com/pilotspace/laya-codex/releases/download/v0.1.2/moon-v0.1.2-aarch64-apple-darwin.tar.gz"
        sha256 "65d366a5bd9302fe2caf210a6db4365cdd681304393ba0307d7e5345dda0df1c"
      end
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/pilotspace/laya-codex/releases/download/v0.1.2/laya-v0.1.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a527bd0295d036c4e1adeabd943d90ee65534123ffbc8edcf605cd8f3e849bba"

      resource "moon" do
        url "https://github.com/pilotspace/laya-codex/releases/download/v0.1.2/moon-v0.1.2-x86_64-unknown-linux-gnu.tar.gz"
        sha256 "678fead27bc502354178c4ca7f2367ab3ae6b22f532fccdce7a647aeac0da0de"
      end
    end
  end

  def install
    # laya runs `moon` from beside its own binary first, so both live in libexec. Only `laya` is
    # exposed: homebrew-core's unrelated `moon` (moonrepo) owns bin/moon. The wrapper execs the
    # real binary, so laya's own path (and so the moon it picks) is libexec, not the bin symlink.
    libexec.install "laya"
    bin.write_exec_script libexec/"laya"
    pkgshare.install "THIRD-PARTY-LICENSES.txt"
    resource("moon").stage do
      libexec.install "moon"
      (pkgshare/"moon").install "LICENSE", "SOURCE"
    end
  end

  def caveats
    <<~EOS
      The laya-code re-ranker (~850 MB) is not part of this formula. Without it laya ranks
      lexically. To download and verify it into ~/.cache/laya-codex/models/laya-code:
        curl -fsSL https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh -s -- --model-only

      Enable laya in Claude Code, either everywhere with the plugin (inside Claude Code):
        /plugin marketplace add pilotspace/laya-codex
        /plugin install laya-codex@laya-codex
      or per repository (hooks then point at this version's Cellar path, so re-run it after
      `brew upgrade laya`):
        laya init --repo /path/to/repo
      Then check the setup with:
        laya doctor --repo /path/to/repo
    EOS
  end

  test do
    require "json"

    assert_match "Ranked code retrieval", shell_output("#{bin}/laya --help")
    assert_match "Usage: moon", shell_output("#{libexec}/moon --help")

    # Offline self-check in a private LAYA_HOME: nothing is started, no model is needed.
    ENV["LAYA_HOME"] = (testpath/"home").to_s
    ENV["LAYA_NO_MODEL"] = "1"
    ENV["LAYA_MOON_PORT"] = free_port.to_s
    (testpath/"repo").mkpath
    system "git", "-C", testpath/"repo", "init", "--quiet"
    # Exit status is 1 because the test repo has no hooks; the report must still parse, and it
    # must pick the bundled moon beside laya (not another `moon` on PATH) and a usable LAYA_HOME.
    report = JSON.parse(shell_output("#{bin}/laya doctor --json --repo #{testpath}/repo", 1))
    checks = report["checks"].to_h { |c| [c["name"], c] }
    assert_equal "pass", checks["moon"]["level"]
    assert_match "#{libexec}/moon", checks["moon"]["detail"]
    assert_equal "pass", checks["home"]["level"]
    assert_equal "fail", checks["hooks"]["level"]
  end
end
