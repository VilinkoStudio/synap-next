import 'package:flutter/material.dart';

/// web 自有扁平设计语言：和产品匹配，不抄任何一家。
///
/// 扁平三原则：零阴影（elevation 全 0）、1px 边框代替投影、
/// 实色填充 + 小圆角。以后产品视觉迭代只改这里的 token。
class FlatTokens {
  static const seed = Color(0xFF0E7C6B); // 产品青：五端共用
  static const radius = 10.0;
}

ThemeData buildFlatTheme() {
  final scheme = ColorScheme.fromSeed(seedColor: FlatTokens.seed);
  final border = BorderSide(color: scheme.outlineVariant);
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: scheme.surface,
    splashFactory: NoSplash.splashFactory,
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surface,
      elevation: 0,
      scrolledUnderElevation: 0,
      titleTextStyle: TextStyle(
        color: scheme.onSurface,
        fontSize: 18,
        fontWeight: FontWeight.w600,
      ),
    ),
    cardTheme: CardThemeData(
      elevation: 0,
      margin: EdgeInsets.zero,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(FlatTokens.radius),
        side: border,
      ),
    ),
    inputDecorationTheme: InputDecorationTheme(
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(FlatTokens.radius),
      ),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: FilledButton.styleFrom(
        elevation: 0,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(8),
        ),
      ),
    ),
  );
}

/// 扁平卡片：边框代替阴影。web 分支用它，别的端走 Material Card。
class FlatCard extends StatelessWidget {
  const FlatCard({super.key, required this.child, this.padding = const EdgeInsets.all(12)});

  final Widget child;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      decoration: BoxDecoration(
        color: scheme.surfaceContainerLowest,
        border: Border.all(color: scheme.outlineVariant),
        borderRadius: BorderRadius.circular(FlatTokens.radius),
      ),
      padding: padding,
      child: child,
    );
  }
}

/// 扁平按钮：实色填充、无阴影、小圆角。
class FlatButton extends StatelessWidget {
  const FlatButton({super.key, required this.label, required this.onPressed});

  final String label;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) {
    return FilledButton(onPressed: onPressed, child: Text(label));
  }
}
