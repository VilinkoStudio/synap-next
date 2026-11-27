import 'package:flutter/material.dart';

import 'debug_preview.dart';
import 'rfw_demo.dart';

/// 调试设置页：宫格切换预览平台，点选即时生效。
/// 入口只在 debug 显示（WorkbenchShell 的 onOpenSettings 传 null 即隐藏）。
class SettingsPage extends StatelessWidget {
  const SettingsPage({super.key});

  static const _items = [
    (PreviewTarget.system, Icons.settings_suggest),
    (PreviewTarget.android, Icons.android),
    (PreviewTarget.ios, Icons.phone_iphone),
    (PreviewTarget.macos, Icons.laptop_mac),
    (PreviewTarget.windows, Icons.desktop_windows),
    (PreviewTarget.web, Icons.web),
    (PreviewTarget.linux, Icons.computer),
  ];

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('设置')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text('调试 · 预览平台', style: Theme.of(context).textTheme.titleMedium),
          const SizedBox(height: 4),
          const Text('只换皮肤与壳结构，不动数据。返回即看效果。'),
          const SizedBox(height: 12),
          ValueListenableBuilder<PreviewTarget>(
            valueListenable: previewTarget,
            builder: (context, current, _) {
              return GridView.count(
                crossAxisCount: 3,
                shrinkWrap: true,
                physics: const NeverScrollableScrollPhysics(),
                mainAxisSpacing: 12,
                crossAxisSpacing: 12,
                children: [
                  for (final (target, icon) in _items)
                    _Tile(
                      label: target.label,
                      icon: icon,
                      selected: current == target,
                      onTap: () => previewTarget.value = target,
                    ),
                ],
              );
            },
          ),
          const SizedBox(height: 24),
          Text('插件 UI 验证', style: Theme.of(context).textTheme.titleMedium),
          const SizedBox(height: 4),
          const Text('RFW 描述渲染 Reader 槽位，数据是假投影，点按钮看回写事件。'),
          const SizedBox(height: 12),
          FilledButton.tonal(
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute(builder: (_) => const RfwReaderDemo()),
            ),
            child: const Text('打开 RFW Reader 演示'),
          ),
        ],
      ),
    );
  }
}

class _Tile extends StatelessWidget {
  const _Tile({
    required this.label,
    required this.icon,
    required this.selected,
    required this.onTap,
  });

  final String label;
  final IconData icon;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Card(
      elevation: 0,
      color: selected ? scheme.primaryContainer : null,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(
          color: selected ? scheme.primary : scheme.outlineVariant,
          width: selected ? 2 : 1,
        ),
      ),
      child: InkWell(
        borderRadius: BorderRadius.circular(12),
        onTap: onTap,
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            Icon(icon,
                color: selected ? scheme.onPrimaryContainer : scheme.onSurfaceVariant),
            const SizedBox(height: 8),
            Text(label, style: Theme.of(context).textTheme.bodyMedium),
          ],
        ),
      ),
    );
  }
}
