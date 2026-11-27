import 'package:flutter/foundation.dart';

import 'debug_preview.dart';

/// 平台与尺寸判断：全仓唯一出处.
///
/// - 布局按**宽度**分（compact / expanded），不按 OS 分。
/// - 皮肤按五端路由：web 扁平 / Windows fluent_ui /
///   iOS·macOS Cupertino / Android·其他 M3E。
/// - 调试时设置页可覆盖（previewTarget），整机实时换肤。
const double kCompactMaxWidth = 700;

/// 紧凑布局？手机竖屏 / 窄窗 / 手机浏览器都走这条。
bool isCompactWidth(double width) => width < kCompactMaxWidth;

/// 当前预览覆盖；system 表示跟随本机。
PreviewTarget get _preview => previewTarget.value;

/// 生效中的平台（调试覆盖优先）。
TargetPlatform get _platform => switch (_preview) {
      PreviewTarget.android => TargetPlatform.android,
      PreviewTarget.ios => TargetPlatform.iOS,
      PreviewTarget.macos => TargetPlatform.macOS,
      PreviewTarget.windows => TargetPlatform.windows,
      PreviewTarget.linux => TargetPlatform.linux,
      PreviewTarget.system || PreviewTarget.web => defaultTargetPlatform,
    };

/// web：自有扁平设计语言。预览 web 时只换皮肤，结构沿用本机宽度。
bool get useFlatWeb {
  if (_preview != PreviewTarget.system) return _preview == PreviewTarget.web;
  return kIsWeb;
}

/// Windows：fluent_ui，真 WinUI。
bool get useFluent => !useFlatWeb && _platform == TargetPlatform.windows;

/// Linux：yaru（Ubuntu/GNOME 味，真 GTK 体系主题）。
bool get useYaru => !useFlatWeb && _platform == TargetPlatform.linux;

/// Cupertino 系（iOS 习语；macOS 以后换 macos_ui，分支点不变）。
bool get useCupertinoControls =>
    !useFlatWeb &&
    (_platform == TargetPlatform.iOS || _platform == TargetPlatform.macOS);
