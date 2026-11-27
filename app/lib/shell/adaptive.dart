import 'package:fluent_ui/fluent_ui.dart' as fluent;
import 'package:flutter/cupertino.dart';
import 'package:flutter/material.dart';

import 'flat_kit.dart';
import 'platform.dart';

/// 换肤门面：调用方永远只认识 PlatformButton / PlatformCard，不认识三方包。
///
/// - web → 自有扁平 FlatButton
/// - Windows → fluent_ui FilledButton（真 WinUI）
/// - iOS/macOS → CupertinoButton（macOS 以后换 macos_ui，调用方不动）
/// - Android/其他 → Material FilledButton
class PlatformButton extends StatelessWidget {
  const PlatformButton({super.key, required this.label, required this.onPressed});

  final String label;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) {
    if (useFlatWeb) return FlatButton(label: label, onPressed: onPressed);
    if (useFluent) return fluent.FilledButton(onPressed: onPressed, child: Text(label));
    if (useCupertinoControls) {
      return CupertinoButton.filled(onPressed: onPressed, child: Text(label));
    }
    return FilledButton(onPressed: onPressed, child: Text(label));
  }
}

/// 卡片门面：web 用扁平边框卡，其他端用 Material Card。
/// Windows 内容区以后 Fluent 化时，只改这里。
class PlatformCard extends StatelessWidget {
  const PlatformCard({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    if (useFlatWeb) return FlatCard(child: child);
    return Card(child: Padding(padding: const EdgeInsets.all(12), child: child));
  }
}
