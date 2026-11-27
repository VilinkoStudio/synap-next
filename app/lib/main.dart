import 'package:fluent_ui/fluent_ui.dart' as fluent;
import 'package:fluent_ui/fluent_ui.dart' show FluentLocalizations;
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:yaru/yaru.dart';
import 'shell/debug_preview.dart';
import 'shell/flat_kit.dart';
import 'shell/platform.dart';
import 'shell/settings_page.dart';
import 'shell/slots.dart';
import 'shell/workbench_shell.dart';

/// synap-next Flutter 宿主：状态在 page，样式在门面，组装在 shell。
/// 对应 docs/003-product-shape.md：Capture / Reader / Actions / Context / Workspace。
///
/// 五端路由：web→自有扁平主题 / Windows→fluent_ui /
/// iOS·macOS→Cupertino / Android→M3E。Rust 绑定在 bridge_generated/，
/// `flutter run/build/test` 经 hook/build.dart 自动编 Rust。
///
/// 预览别的端有两种方式（二选一，不叠加）：
/// - 设置页宫格（debug 包右上齿轮）：点选即时换肤；
/// - 启动参数：flutter run --dart-define=PREVIEW_PLATFORM=windows。
void main() {
  const preview = String.fromEnvironment('PREVIEW_PLATFORM', defaultValue: '');
  if (preview.isNotEmpty) {
    debugDefaultTargetPlatformOverride = switch (preview) {
      'windows' => TargetPlatform.windows,
      'macos' => TargetPlatform.macOS,
      'ios' => TargetPlatform.iOS,
      'android' => TargetPlatform.android,
      'linux' => TargetPlatform.linux,
      _ => null,
    };
  }
  runApp(const SynapApp());
}

class SynapApp extends StatelessWidget {
  const SynapApp({super.key});

  @override
  Widget build(BuildContext context) {
    // previewTarget 一变，整机 MaterialApp 重建，主题与壳同步切换。
    // FluentTheme 置顶：任何位置的 fluent widget（含未来弹窗/新路由）
    // 都能找到主题，不依赖壳分支包没包住。
    return ValueListenableBuilder(
      valueListenable: previewTarget,
      builder: (context, _, _) {
        return fluent.FluentTheme(
          data: fluent.FluentThemeData.light(),
          child: MaterialApp(
            title: 'synap-next',
            theme: useFlatWeb
                ? buildFlatTheme()
                : useYaru
                    ? yaruLight
                    : ThemeData(useMaterial3: true, colorSchemeSeed: FlatTokens.seed),
            localizationsDelegates: const [
              ...GlobalMaterialLocalizations.delegates,
              FluentLocalizations.delegate,
            ],
            home: const WorkbenchPage(),
          ),
        );
      },
    );
  }
}

class WorkbenchPage extends StatefulWidget {
  const WorkbenchPage({super.key});

  @override
  State<WorkbenchPage> createState() => _WorkbenchPageState();
}

class _WorkbenchPageState extends State<WorkbenchPage> {
  String _status = 'bridge 未接入（just gen 已生成，待接 RustLib.init）';
  final List<String> _records = [];

  void _capture() {
    // 槽位：Capture。以后调 bridge_put_record，经 runtime.dispatch 写中层记录。
    setState(() {
      _records.add('草稿 ${_records.length + 1} · 仅本地占位');
      _status = '已记录一条（本地占位，未进 Rust）';
    });
  }

  void _openSettings() {
    Navigator.of(context).push(
      MaterialPageRoute(builder: (_) => const SettingsPage()),
    );
  }

  @override
  Widget build(BuildContext context) {
    return WorkbenchShell(
      // release 包不设入口，调试宫格留在 debug。
      onOpenSettings: kDebugMode ? _openSettings : null,
      capture: CaptureBar(
        hint: '记下、打开最近的东西、搜索——零插件时也能用。',
        onCapture: _capture,
      ),
      reader: ReaderPane(records: _records),
      contextPane: const ContextPane(),
      actions: ActionsPane(status: _status),
    );
  }
}
