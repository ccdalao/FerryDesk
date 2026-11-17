import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:screenferry_settings/ipc/launcher.dart';

void main() {
  late Directory root;

  setUp(() => root = Directory.systemTemp.createTempSync('sf-launcher-'));
  tearDown(() => root.deleteSync(recursive: true));

  String settingsExe(String app) => '$app/Contents/Helpers/ScreenFerry.app/Contents/MacOS/ScreenFerry';

  test('装在 ScreenFerry.app 里时找到外层应用', () {
    final app = '${root.path}/ScreenFerry.app';
    File('$app/Contents/MacOS/screenferry').createSync(recursive: true);
    expect(daemonLocation(settingsExe(app)), app);
  }, skip: Platform.isWindows);

  test('Windows 安装目录中的设置界面找到同包后台', () {
    final app = root.path;
    final executable = '$app${Platform.pathSeparator}settings${Platform.pathSeparator}screenferry-settings.exe';
    final daemon = File('$app${Platform.pathSeparator}screenferry.exe');
    daemon.createSync();
    expect(daemonLocation(executable), daemon.path);
    daemon.deleteSync();
    expect(daemonLocation(executable), isNull);
  }, skip: !Platform.isWindows);

  test('外层应用里没有后台程序时找不到', () {
    final app = '${root.path}/ScreenFerry.app';
    Directory('$app/Contents/MacOS').createSync(recursive: true);
    expect(daemonLocation(settingsExe(app)), isNull);
  });

  test('开发时直接运行的界面不在安装包里', () {
    expect(
      daemonLocation('${root.path}/build/macos/Build/Products/Release/ScreenFerry.app/Contents/MacOS/ScreenFerry'),
      isNull,
    );
  });
}
