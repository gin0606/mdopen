import AppKit
import OSLog

/// Finder から渡されたファイルを bundle 同梱の `mdhtml` で変換し、返ってきたページを
/// 既定のブラウザで開いて終了するランチャ。
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
            switch convert(url.path) {
            case .converted(let page):
                if !NSWorkspace.shared.open(page) {
                    report("\(page.path) をブラウザで開けません", for: url)
                }
            case .failed(let message):
                report(message, for: url)
            }
        }

        NSApp.terminate(nil)
    }

    /// 同梱の `mdhtml` を起動して待ち、標準出力に返ってきた変換結果のページを渡す。
    private func convert(_ path: String) -> Conversion {
        let tool = Bundle.main.bundleURL
            .appendingPathComponent("Contents/MacOS/mdhtml")

        let process = Process()
        process.executableURL = tool
        process.arguments = [path]

        let output = Pipe()
        let errors = Pipe()
        process.standardOutput = output
        process.standardError = errors

        do {
            try process.run()
        } catch {
            return .failed("mdhtml を起動できません: \(error.localizedDescription)")
        }

        // 読み手を立てるのは run の後。起動に失敗すると書き手が現れず、読み切りが返らない。
        let producedPath = PipeReader(output)
        let diagnostics = PipeReader(errors)

        // waitUntilExit より先に読み切らないと、パイプが埋まったときに詰まる。
        let produced = producedPath.wait()
        let diagnostic = diagnostics.wait()
        process.waitUntilExit()

        let page = String(decoding: produced.data, as: UTF8.self)
            .trimmingCharacters(in: .newlines)
        let message = String(decoding: diagnostic.data, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)

        // mdhtml は成功時にも警告を出す。LaunchServices 経由の起動では標準エラーが捨てられるので、
        // ダイアログで遮らずに残すには unified log に書くしかない。
        if !message.isEmpty {
            Launcher.log.notice("\(message, privacy: .public)")
        }

        // 読み取りの失敗より、mdhtml 自身が出した理由を先に見る。両方起きたときは
        // 後者のほうが利用者にとって行動につながる。
        guard process.terminationStatus == 0, process.terminationReason == .exit else {
            let ended = process.terminationReason == .uncaughtSignal
                ? "シグナル \(process.terminationStatus)"
                : "終了コード \(process.terminationStatus)"
            return .failed(message.isEmpty ? "mdhtml が\(ended)で終了しました" : message)
        }

        if let failure = produced.failure ?? diagnostic.failure {
            return .failed("mdhtml の出力を読めません: \(failure.localizedDescription)")
        }

        guard !page.isEmpty else {
            return .failed("mdhtml が変換結果のパスを返しませんでした")
        }

        return .converted(URL(fileURLWithPath: page))
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

private enum Conversion {
    case converted(URL)
    case failed(String)
}

/// パイプを 1 本、専用のキューで読み切る。標準出力と標準エラーを片方ずつ読み切ると、
/// 先に埋まったほうで変換器が止まるので、2 本を並行に流す。
private final class PipeReader {
    private var data = Data()
    private var failure: Error?
    private let group = DispatchGroup()

    init(_ pipe: Pipe) {
        DispatchQueue.global().async(group: group) { [self] in
            // readDataToEndOfFile は読み取り失敗を Swift から捕まえられない例外で伝える。
            // LSUIElement なのでそのまま落ちると利用者には何も起きなかったように見える。
            do {
                data = try pipe.fileHandleForReading.readToEnd() ?? Data()
            } catch {
                failure = error
            }
        }
    }

    /// 読み切るまで待つ。失敗していたら、そこまでに受け取れた分と一緒に理由も返す。
    func wait() -> (data: Data, failure: Error?) {
        group.wait()
        return (data, failure)
    }
}

let application = NSApplication.shared
let launcher = Launcher()
application.delegate = launcher
application.run()
