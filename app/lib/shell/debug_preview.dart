import 'package:flutter/foundation.dart';

/// 调试预览：设置页宫格切换，整机实时换肤，无需重编。
///
/// release 包里设置入口不显示（main 传 null），这个 notifier 恒为 system。
enum PreviewTarget { system, android, ios, macos, windows, web, linux }

extension PreviewTargetLabel on PreviewTarget {
  String get label => switch (this) {
        PreviewTarget.system => '跟随系统',
        PreviewTarget.android => 'Android',
        PreviewTarget.ios => 'iOS',
        PreviewTarget.macos => 'macOS',
        PreviewTarget.windows => 'Windows',
        PreviewTarget.web => 'Web 扁平',
        PreviewTarget.linux => 'Linux Yaru',
      };
}

final previewTarget = ValueNotifier<PreviewTarget>(PreviewTarget.system);
