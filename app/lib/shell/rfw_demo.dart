import 'package:flutter/material.dart';
import 'package:rfw/formats.dart' show parseLibraryFile;
import 'package:rfw/rfw.dart';
import 'synap_widgets.dart';

/// RFW 验证：Reader 槽位的一份 UI 描述 + 假投影数据 + 事件回写。
///
/// 对应架构问题：插件 UI 是编译进宿主，还是运行时下发的描述由 Flutter 解释。
/// 这里演示后者——描述是纯数据（将来可随数据插件同步），渲染与事件由宿主承接。
class RfwReaderDemo extends StatefulWidget {
  const RfwReaderDemo({super.key});

  @override
  State<RfwReaderDemo> createState() => _RfwReaderDemoState();
}

class _RfwReaderDemoState extends State<RfwReaderDemo> {
  static const LibraryName coreName = LibraryName(<String>['core', 'widgets']);
  static const LibraryName materialName = LibraryName(<String>['core', 'material']);
  static const LibraryName synapName = LibraryName(<String>['synap', 'widgets']);
  static const LibraryName demoName = LibraryName(<String>['demo', 'reader']);

  /// 插件下发的 UI 描述：纯文本，运行时解释。宿主只注册控件，不认识具体插件。
  static final RemoteWidgetLibrary _library = parseLibraryFile('''
    import core.widgets;
    import core.material;
    import synap.widgets;
    widget root = ReaderCard(
      child: Padding(
        padding: [16, 16, 16, 16],
        child: Column(
          crossAxisAlignment: "start",
          children: [
            Title(text: [data.note.title]),
            SizedBox(height: 8),
            Body(text: [data.note.body]),
            SizedBox(height: 8),
            Wrap(
              spacing: 8,
              children: [
                ...for tag in data.note.tags: Tag(text: ["#", tag]),
              ],
            ),
            SizedBox(height: 12),
            PrimaryButton(
              label: switch data.note.done { true: "已完成，点我重开", false: "标记完成" },
              onPressed: event 'toggle-done' { entity: data.note.entity },
            ),
          ],
        ),
      ),
    );
  ''');

  final Runtime _runtime = Runtime();
  final DynamicContent _data = DynamicContent();

  // 演示用本机状态：真正接线后这里换成 Trait 投影行，_pushNote 换成订阅回调。
  bool _done = false;

  void _pushNote() {
    _data.update('note', <String, Object>{
      'entity': 'entity-demo-001',
      'title': 'RFW Reader 演示',
      'body': '这段界面由插件下发的描述渲染，不是编译进宿主的 Dart 代码。',
      'done': _done,
      'tags': <Object>['rfw', 'reader', 'demo'],
    });
  }

  @override
  void initState() {
    super.initState();
    _runtime.update(coreName, createCoreWidgets());
    _runtime.update(materialName, createMaterialWidgets());
    _runtime.update(synapName, synapWidgets());
    _runtime.update(demoName, _library);
    _pushNote();
  }

  void _onEvent(String name, DynamicMap arguments) {
    if (name == 'toggle-done') {
      // 写回走 Trait 标准接口（set_done），不直调数据插件——演示里直接翻数据。
      _done = !_done;
      _pushNote();
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('trait.Checkable.set_done(${arguments['entity']})')),
        );
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('RFW Reader 演示')),
      body: SingleChildScrollView(
        child: RemoteWidget(
          runtime: _runtime,
          data: _data,
          widget: const FullyQualifiedWidgetName(demoName, 'root'),
          onEvent: _onEvent,
        ),
      ),
    );
  }
}
