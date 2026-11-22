import 'package:flutter/material.dart';

import 'adaptive.dart';

/// 槽位内容：三端…五端同一份，里面零平台分支.
///
/// 槽位即 docs/003 说的语义槽：Capture / Reader / Actions / Context。
/// 样式差异在 adaptive.dart 的门面里解决，这里只管信息结构。
/// 需要平台行为（分享/文件/震动）时调注入的回调，不许写 Platform.isX。

/// 槽位：Capture，用户正在记录或导入。
class CaptureBar extends StatelessWidget {
  const CaptureBar({super.key, required this.hint, required this.onCapture});

  final String hint;
  final VoidCallback onCapture;

  @override
  Widget build(BuildContext context) {
    return PlatformCard(
      child: Row(
        children: [
          Expanded(child: Text(hint, style: Theme.of(context).textTheme.bodyMedium)),
          PlatformButton(label: '记一条', onPressed: onCapture),
        ],
      ),
    );
  }
}

/// 槽位：Reader，打开一个对象，插件决定怎么呈现。
/// 注意 ListTile 无 onTap（无水波纹），放在 Fluent 页里也安全。
class ReaderPane extends StatelessWidget {
  const ReaderPane({super.key, required this.records});

  final List<String> records;

  @override
  Widget build(BuildContext context) {
    return PlatformCard(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('当前内容', style: Theme.of(context).textTheme.titleMedium),
          const SizedBox(height: 8),
          if (records.isEmpty)
            const Text('空：记一条，或等 Rust 投影接进来。')
          else
            for (final r in records) ListTile(leading: const Icon(Icons.note), title: Text(r)),
        ],
      ),
    );
  }
}

/// 槽位：Context，来源、关系、版本、分析贴在当前对象旁边。
class ContextPane extends StatelessWidget {
  const ContextPane({super.key});

  @override
  Widget build(BuildContext context) {
    return const PlatformCard(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('旁边'),
          SizedBox(height: 8),
          Text('来源 / 版本 / 分析以后贴在这里，不另开插件首页。'),
        ],
      ),
    );
  }
}

/// 槽位：Actions，选中对象后插件能做的动作；状态行先放这里。
class ActionsPane extends StatelessWidget {
  const ActionsPane({super.key, required this.status});

  final String status;

  @override
  Widget build(BuildContext context) {
    return PlatformCard(child: Text(status));
  }
}
