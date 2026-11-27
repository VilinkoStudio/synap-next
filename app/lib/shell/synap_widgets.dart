import 'package:flutter/material.dart';
import 'package:rfw/rfw.dart';

import 'adaptive.dart';

/// 宿主设计系统：RFW 本地库 `synap.widgets`。
///
/// 插件描述只引用这里的名字（Title/Body/Tag/PrimaryButton/ReaderCard），
/// 具体长相由当前主题 + 平台分支决定——宫格切皮肤，RFW 跟着换。
LocalWidgetLibrary synapWidgets() => LocalWidgetLibrary(<String, LocalWidgetBuilder>{
      'Title': (BuildContext context, DataSource source) {
        return Text(
          _text(source, 'text'),
          style: Theme.of(context).textTheme.titleLarge,
        );
      },
      'Body': (BuildContext context, DataSource source) {
        return Text(
          _text(source, 'text'),
          style: Theme.of(context).textTheme.bodyMedium,
        );
      },
      'Tag': (BuildContext context, DataSource source) {
        return Chip(label: Text(_text(source, 'text')));
      },
      'PrimaryButton': (BuildContext context, DataSource source) {
        return PlatformButton(
          label: source.v<String>(['label']) ?? '',
          onPressed: source.voidHandler(['onPressed']),
        );
      },
      'ReaderCard': (BuildContext context, DataSource source) {
        return PlatformCard(child: source.child(['child']));
      },
    });

/// 与 core Text 同规则：字符串或列表拼接。
String _text(DataSource source, String key) {
  final String? direct = source.v<String>([key]);
  if (direct != null) return direct;
  final StringBuffer out = StringBuffer();
  final int count = source.length([key]);
  for (int i = 0; i < count; i++) {
    out.write(source.v<String>([key, i]) ?? '');
  }
  return out.toString();
}
