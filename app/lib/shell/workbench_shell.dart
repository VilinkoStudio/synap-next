import 'package:fluent_ui/fluent_ui.dart' as fluent;
import 'package:flutter/material.dart';

import 'platform.dart';

/// 工作台骨架：唯一的分支收敛点.
///
/// - Windows → FluentTheme + NavigationView + ScaffoldPage（真 WinUI 壳，
///   内容槽位复用同一批 widget；内容区 Fluent 化以后再说）。
/// - 其他端 → Scaffold + 宽窄分栏（web 走扁平主题，结构与桌面相同）。
/// 槽位内容从外部以 Widget 传进来，壳只管摆法。
/// onOpenSettings 为 null 时不显示设置入口（release 包即如此）。
class WorkbenchShell extends StatelessWidget {
  const WorkbenchShell({
    super.key,
    required this.capture,
    required this.reader,
    required this.contextPane,
    required this.actions,
    this.onOpenSettings,
  });

  final Widget capture;
  final Widget reader;
  final Widget contextPane;
  final Widget actions;
  final VoidCallback? onOpenSettings;

  @override
  Widget build(BuildContext context) {
    if (useFluent) {
      // FluentTheme 已置顶到 MaterialApp 之上（main.dart），这里只管壳结构。
      return fluent.NavigationView(
        content: fluent.ScaffoldPage(
          header: fluent.PageHeader(
            title: const Text('synap-next 工作台'),
            commandBar: onOpenSettings == null
                ? null
                : fluent.Button(
                    onPressed: onOpenSettings,
                    child: const Text('设置'),
                  ),
          ),
          content: _Body(
            capture: capture,
            reader: reader,
            contextPane: contextPane,
            actions: actions,
          ),
        ),
      );
    }
    return Scaffold(
      appBar: AppBar(
        title: const Text('synap-next 工作台'),
        actions: [
          if (onOpenSettings != null)
            IconButton(
              icon: const Icon(Icons.settings_outlined),
              tooltip: '设置',
              onPressed: onOpenSettings,
            ),
        ],
      ),
      body: _Body(
        capture: capture,
        reader: reader,
        contextPane: contextPane,
        actions: actions,
      ),
    );
  }
}

/// 宽窄分栏：五端共用。只按宽度分，不按 OS 分。
class _Body extends StatelessWidget {
  const _Body({
    required this.capture,
    required this.reader,
    required this.contextPane,
    required this.actions,
  });

  final Widget capture;
  final Widget reader;
  final Widget contextPane;
  final Widget actions;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        if (!isCompactWidth(constraints.maxWidth)) {
          return Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              children: [
                capture,
                const SizedBox(height: 12),
                Expanded(
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Expanded(flex: 3, child: reader),
                      const SizedBox(width: 12),
                      Expanded(
                        flex: 2,
                        child: ListView(children: [contextPane, const SizedBox(height: 12), actions]),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          );
        }
        return ListView(
          padding: const EdgeInsets.all(16),
          children: [
            capture,
            const SizedBox(height: 12),
            reader,
            const SizedBox(height: 12),
            contextPane,
            const SizedBox(height: 12),
            actions,
          ],
        );
      },
    );
  }
}
