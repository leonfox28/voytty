import SwiftUI

@main
struct VoyttyApp: App {
    @StateObject private var model = AppModel(diagnostics: AppDiagnostics())
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView(model: model)
                .preferredColorScheme(model.preferences.theme == "dark" ? .dark : model.preferences.theme == "light" ? .light : nil)
                .tint(AppTheme.primary)
                .task { await model.start() }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    model.sceneChanged(phase)
                    if phase == .active { Task { await model.networkChanged() } }
                }
        }
    }
}

struct RootView: View {
    @ObservedObject var model: AppModel
    private var strings: AppStrings { model.strings }
    @Environment(\.colorScheme) private var colorScheme

    private var path: Binding<[AppRoute]> {
        Binding(get: { model.route == .home ? [] : [model.route] }, set: { destinations in
            // A committed system Back/edge swipe owns the same detach/cancel
            // boundary as explicit Home. A cancelled swipe leaves it untouched.
            if destinations.isEmpty, model.route != .home { model.goHome() }
        })
    }

    var body: some View {
        NavigationStack(path: path) {
            HomeScreen(model: model)
                .background(AppTheme.background)
                .navigationTitle(strings("主机列表"))
                .navigationBarTitleDisplayMode(.inline)
                .toolbar(.hidden, for: .navigationBar)
                .navigationDestination(for: AppRoute.self) { route in
                    Group {
                        switch route {
                        case .home: HomeScreen(model: model)
                        case .settings: SettingsScreen(model: model)
                        case .scanner: ScannerScreen(model: model)
                        case .terminal: TerminalScreen(model: model)
                        }
                    }
                    .background(AppTheme.background)
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar(.visible, for: .navigationBar)
                    .toolbarBackground(AppTheme.chrome, for: .navigationBar)
                    .toolbarBackground(.visible, for: .navigationBar)
                }
        }
        .environment(\.locale, model.strings.locale)
        .foregroundStyle(AppTheme.foreground)
        .background(AppTheme.background.ignoresSafeArea())
        .onChange(of: colorScheme, initial: true) { _, scheme in model.setDark(scheme == .dark) }
    }
}

struct HomeScreen: View {
    @ObservedObject var model: AppModel
    private var strings: AppStrings { model.strings }
    @State private var forgetting: SavedHost?

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            VStack(spacing: 0) {
                HStack(spacing: 14) {
                    IconTile(symbol: "terminal")
                    Text("voytty").font(.system(size: 26, weight: .semibold))
                    Spacer()
                    Button { model.route = .settings } label: {
                        Image(systemName: "gearshape").font(.system(size: 22)).frame(width: 44, height: 44)
                    }.accessibilityLabel(strings("设置")).accessibilityIdentifier("settings")
                }.frame(height: 72)
                Group {
                    if model.busy || !model.ready && model.error == nil { ProgressView().progressViewStyle(.linear) }
                    else { Color.clear }
                }.frame(height: 4)
                if model.ready && model.hosts.isEmpty {
                    VStack(spacing: 12) {
                        IconTile(symbol: "server.rack", size: 72).padding(.bottom, 12)
                        Text(strings("还没有主机")).font(.system(size: 20, weight: .semibold))
                        Text(strings("在电脑上运行下面的命令，然后添加主机。"))
                            .font(.system(size: 14)).foregroundStyle(AppTheme.secondary).multilineTextAlignment(.center)
                        Text("$ voytty pair create").font(.system(size: 14, design: .monospaced))
                            .padding(.horizontal, 14).padding(.vertical, 10)
                            .background(AppTheme.elevated, in: RoundedRectangle(cornerRadius: 10)).padding(.top, 8)
                    }.frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 8) {
                            if let host = model.recentHost {
                                Button { Task { await model.select(host, recent: true) } } label: {
                                    HStack(spacing: 12) {
                                        VStack(alignment: .leading, spacing: 6) {
                                            Text(strings("最近连接")).font(.system(size: 12, weight: .semibold)).opacity(0.72)
                                            Text(host.name).font(.system(size: 26, weight: .semibold)).lineLimit(1)
                                            Text(strings("继续上次的会话")).font(.system(size: 14)).opacity(0.72)
                                        }.frame(maxWidth: .infinity, alignment: .leading)
                                        Image(systemName: "arrow.right").font(.system(size: 22))
                                            .frame(width: 48, height: 48)
                                            .foregroundStyle(AppTheme.onPrimary).background(AppTheme.primary, in: Circle())
                                    }.padding(20).foregroundStyle(AppTheme.onPrimaryContainer)
                                        .background(AppTheme.primaryContainer, in: RoundedRectangle(cornerRadius: 22))
                                }.buttonStyle(.plain).disabled(model.busy).accessibilityIdentifier("recentConnection")
                                    .padding(.bottom, 12)
                            }
                            Text(strings("已保存的主机")).font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(AppTheme.secondary).padding(.vertical, 8)
                            ForEach(model.hosts) { host in
                                Button { Task { await model.select(host) } } label: {
                                    HStack(spacing: 16) {
                                        Text(String(host.name.trimmingCharacters(in: .whitespaces).prefix(1)).uppercased())
                                            .font(.system(size: 18, weight: .medium, design: .monospaced))
                                            .foregroundStyle(AppTheme.primary).frame(width: 40, height: 40)
                                            .background(AppTheme.elevated, in: RoundedRectangle(cornerRadius: 10))
                                        Text(host.name).font(.system(size: 15, weight: .medium)).lineLimit(1)
                                            .frame(maxWidth: .infinity, alignment: .leading)
                                        Image(systemName: "chevron.right").font(.system(size: 14)).foregroundStyle(AppTheme.secondary)
                                    }.padding(.horizontal, 16).padding(.vertical, 14)
                                        .background(AppTheme.surface, in: RoundedRectangle(cornerRadius: 16))
                                }.buttonStyle(.plain).disabled(model.busy)
                                    .contextMenu { Button(strings("移除主机"), systemImage: "trash", role: .destructive) { forgetting = host } }
                                    .accessibilityIdentifier("host-\(host.id)")
                            }
                        }.padding(.top, 12).padding(.bottom, 96)
                    }
                }
                if let error = model.error {
                    ErrorBanner(text: error, dismiss: { model.error = nil }).padding(.trailing, 76).padding(.bottom, 12)
                }
                if !model.ready && !model.busy {
                    Button(strings("重试初始化")) { Task { await model.start() } }.padding(.bottom, 16)
                }
            }.padding(.horizontal, 20)
            Button { model.error = nil; model.route = .scanner } label: {
                Image(systemName: "plus").font(.system(size: 28, weight: .medium))
                    .frame(width: 64, height: 64).foregroundStyle(AppTheme.onPrimary)
                    .background(AppTheme.primary, in: Circle()).shadow(color: .black.opacity(0.18), radius: 6, y: 3)
            }.buttonStyle(.plain).padding(20).disabled(!model.ready || model.busy)
                .accessibilityLabel(strings("添加主机")).accessibilityIdentifier("addHost")
        }
        .alert(strings("移除主机“%@”？", forgetting?.name ?? ""), isPresented: Binding(
            get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }), presenting: forgetting) { host in
                Button(strings("取消"), role: .cancel) { forgetting = nil }
                    .accessibilityIdentifier("cancelForgetHost")
                Button(strings("移除主机"), role: .destructive) { Task { await model.forget(host) } }
                    .accessibilityIdentifier("confirmForgetHost")
            } message: { _ in Text(strings("仅移除本机保存的连接，远端会话继续运行。重新连接需要再次配对。")) }
    }
}

struct SettingsScreen: View {
    @ObservedObject var model: AppModel
    private var strings: AppStrings { model.strings }
    @State private var fontSize = 15.0
    @State private var showingLicenses = false

    var body: some View {
        Form {
            Section(strings("通用")) {
                VStack(alignment: .leading, spacing: 8) {
                    Text(strings("语言"))
                    Picker(strings("语言"), selection: Binding(get: { model.preferences.language }, set: {
                        var preferences = model.preferences; preferences.language = $0; model.setPreferences(preferences)
                    })) {
                        Text(strings("跟随系统")).tag("system")
                        Text("中文").tag("zh")
                        Text("English").tag("en")
                    }.pickerStyle(.segmented).disabled(!model.ready).accessibilityIdentifier("languageChoice")
                }
                VStack(alignment: .leading, spacing: 8) {
                    Text(strings("主题"))
                    Picker(strings("主题"), selection: Binding(get: { model.preferences.theme }, set: {
                        var preferences = model.preferences; preferences.theme = $0; model.setPreferences(preferences)
                    })) {
                        Text(strings("跟随系统")).tag("system"); Text(strings("深色")).tag("dark"); Text(strings("浅色")).tag("light")
                    }.pickerStyle(.segmented).disabled(!model.ready).accessibilityIdentifier("themeChoice")
                }
            }
            Section(strings("终端")) {
                LabeledContent(strings("文字大小")) {
                    Text("\(Int(fontSize))").monospacedDigit().accessibilityIdentifier("fontSizeValue")
                }
                Slider(value: $fontSize, in: 10...20, step: 1) { editing in
                    if !editing {
                        var preferences = model.preferences; preferences.fontSize = Int(fontSize); model.setPreferences(preferences)
                    }
                }.accessibilityLabel(strings("文字大小")).disabled(!model.ready).accessibilityIdentifier("fontSize")
                Text(verbatim: "$ echo \"Hello World!\"\nHello World!")
                    .font(Font(TerminalFont.font(size: fontSize)))
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 8)
            }
            NotificationSettings(model: model, notifications: model.notifications)
            if let diagnostics = model.diagnostics {
                DiagnosticsSettings(diagnostics: diagnostics, strings: strings)
            }
            Section(strings("关于")) {
                LabeledContent(strings("版本"), value: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "—")
                Link(destination: URL(string: "https://github.com/leonfox28/voytty")!) {
                    HStack { Text("GitHub"); Spacer(); Image(systemName: "arrow.up.right") }
                }
                Button { showingLicenses = true } label: {
                    HStack { Text(strings("许可与声明")); Spacer(); Image(systemName: "chevron.right") }
                }
            }
            if let error = model.error {
                Section {
                    Text(error).foregroundStyle(AppTheme.error)
                    Button(strings("关闭提示")) { model.error = nil }
                }
            }
        }.navigationTitle(strings("设置")).accessibilityIdentifier("settingsForm")
        .onAppear { fontSize = Double(model.preferences.fontSize) }
        .background {
            if let diagnostics = model.diagnostics {
                DiagnosticExportPresentation(diagnostics: diagnostics)
            }
        }
        .sheet(isPresented: $showingLicenses) { LicensesScreen(strings: strings) }
    }
}

private struct LicensesScreen: View {
    let strings: AppStrings
    @Environment(\.dismiss) private var dismiss
    private let files = ["Voytty-NOTICE.txt", "Voytty-PolyForm-Noncommercial-1.0.0.txt",
                         "Voytty-Commercial-License.txt", "NOTICE.txt", "LXGWBrightCode-OFL-1.1.txt", "LXGWBrightCode-README.md",
                         "NerdFonts-LICENSE.txt", "NerdFonts-README.md", "UniFFI-MPL-2.0.txt"]
    var body: some View {
        NavigationStack {
            List(files, id: \.self) { file in
                NavigationLink(file) {
                    ScrollView {
                        Text(contents(file)).font(.system(size: 13, design: .monospaced))
                            .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading).padding()
                    }.navigationTitle(file).navigationBarTitleDisplayMode(.inline)
                }
            }.navigationTitle(strings("许可与声明")).navigationBarTitleDisplayMode(.inline)
                .toolbar { Button(strings("完成")) { dismiss() } }
        }
    }
    private func contents(_ name: String) -> String {
        guard let url = Bundle.main.url(forResource: name, withExtension: nil, subdirectory: "licenses"),
              let text = try? String(contentsOf: url, encoding: .utf8) else { return strings("无法读取许可文件。") }
        return text
    }
}
