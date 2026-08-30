import AppKit
import OSLog

/// Finder から渡されたファイルを、bundle 同梱の `mdopen` に流して終了するだけのランチャ。
final class Launcher: NSObject, NSApplicationDelegate {
    /// ファイルを渡されずに直接起動されたときに居座らないためのタイムアウト。
    private static let idleTimeout: TimeInterval = 3

    private static let log = Logger(subsystem: "me.gin0606.mdopen", category: "convert")

    private var receivedFiles = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Apple Event は起動完了の後に届くので、ここで terminate してはいけない。
        DispatchQueue.main.asyncAfter(deadline: .now() + Launcher.idleTimeout) { [weak self] in
            guard let self, !self.receivedFiles else { return }
            NSApp.terminate(nil)
        }
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        receivedFiles = true

        for url in urls where url.isFileURL {
            if let failure = convert(url.path) {
                report(failure, for: url)
            }
        }

        NSApp.terminate(nil)
    }

    /// 同梱の `mdopen` を起動して待つ。失敗したときだけ、利用者に見せるメッセージを返す。
    private func convert(_ path: String) -> String? {
        let tool = Bundle.main.bundleURL
            .appendingPathComponent("Contents/MacOS/mdopen")

        let process = Process()
        process.executableURL = tool
        process.arguments = [path]

        let errors = Pipe()
        process.standardError = errors

        do {
            try process.run()
        } catch {
            return "mdopen を起動できません: \(error.localizedDescription)"
        }

        // waitUntilExit より先に読み切らないと、パイプが埋まったときに詰まる。
        let stderr = errors.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()

        let message = String(data: stderr, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

        // mdopen は成功時にも警告を出す。LaunchServices 経由の起動では標準エラーが捨てられるので、
        // ダイアログで遮らずに残すには unified log に書くしかない。
        if !message.isEmpty {
            Launcher.log.notice("\(message, privacy: .public)")
        }

        guard process.terminationStatus != 0 else { return nil }

        return message.isEmpty
            ? "mdopen が終了コード \(process.terminationStatus) で終了しました"
            : message
    }

    /// LSUIElement なのでウインドウも Dock アイコンも無い。失敗はダイアログでしか伝わらない。
    private func report(_ message: String, for url: URL) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "\(url.lastPathComponent) を開けませんでした"
        alert.informativeText = message
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }
}

let application = NSApplication.shared
let launcher = Launcher()
application.delegate = launcher
application.run()
