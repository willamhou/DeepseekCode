class Deepseek < Formula
  desc "DeepSeek-first terminal code agent"
  homepage "https://github.com/willamhou/DeepSeekCode"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.0/deepseek-macos-arm64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    else
      url "https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.0/deepseek-macos-x64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.0/deepseek-linux-x64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    else
      odie "DeepSeekCode Homebrew formula currently publishes Linux x64 only"
    end
  end

  def install
    binary = Dir["deepseek*/deepseek"].first || "deepseek"
    bin.install binary => "deepseek"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/deepseek version")
    system "#{bin}/deepseek", "doctor", "--json"
  end
end
