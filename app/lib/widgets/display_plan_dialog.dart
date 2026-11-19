import 'package:flutter/material.dart';

import '../ipc/messages.dart';
import '../l10n/l10n.dart';
import '../theme/tokens.dart';
import 'alignment_ruler.dart';
import '../state/app_scope.dart';

Future<DisplayPlan?> showDisplayPlanDialog(
  BuildContext context, {
  required PeerInfo peer,
  required String localName,
  required DisplayExtensionInfo extension,
  required ScreenSide peerSide,
  DisplayPlan? original,
}) => showDialog<DisplayPlan>(
  context: context,
  builder: (_) =>
      DisplayPlanDialog(peer: peer, localName: localName, extension: extension, peerSide: peerSide, original: original),
);

class DisplayPlanDialog extends StatefulWidget {
  const DisplayPlanDialog({
    super.key,
    required this.peer,
    required this.localName,
    required this.extension,
    required this.peerSide,
    this.original,
  });
  final PeerInfo peer;
  final String localName;
  final DisplayExtensionInfo extension;
  final ScreenSide peerSide;
  final DisplayPlan? original;

  @override
  State<DisplayPlanDialog> createState() => _DisplayPlanDialogState();
}

class _DisplayPlanDialogState extends State<DisplayPlanDialog> {
  late final _name = TextEditingController(text: widget.original?.name ?? '');
  late bool _localSource = widget.original?.localSource ?? true;
  late int? _source = widget.original?.sourceScreen;
  late int? _viewer = widget.original?.viewerScreen;
  late ScreenSide _side = widget.original?.side ?? widget.peerSide;
  late DisplayPrimary _primary = widget.original?.primary ?? DisplayPrimary.source;
  late bool _audio = widget.original?.audio ?? widget.extension.audio;
  late EdgeAlignment _alignment = widget.original?.alignment ?? const EdgeAlignment();
  late int _shortcut = widget.original?.shortcut ?? 0;

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  List<DisplayScreen> get _sourceScreens =>
      _localSource ? widget.extension.localScreens : widget.extension.peerScreens[widget.peer.fingerprint] ?? const [];
  List<DisplayScreen> get _viewerScreens =>
      _localSource ? widget.extension.peerScreens[widget.peer.fingerprint] ?? const [] : widget.extension.localScreens;

  Widget _screenPicker(
    String label,
    List<DisplayScreen> screens,
    int? selected,
    ValueChanged<int?> changed,
    String keyName,
  ) {
    final strings = context.l10n;
    return DropdownButtonFormField<int>(
      key: ValueKey('$keyName-$_localSource-$selected'),
      initialValue: selected ?? 0,
      isExpanded: true,
      decoration: InputDecoration(labelText: label),
      items: [
        DropdownMenuItem(value: 0, child: Text(strings.displayPlanAuto)),
        for (final screen in screens)
          DropdownMenuItem(
            value: screen.id,
            child: Text('${strings.displayPlanScreen} ${screen.id} · ${screen.width}×${screen.height}'),
          ),
        if (selected != null && !screens.any((s) => s.id == selected))
          DropdownMenuItem(value: selected, child: Text(strings.displayPlanMissing)),
      ],
      onChanged: (id) => setState(() => changed(id == 0 ? null : id)),
    );
  }

  ScreenSide _opposite(ScreenSide side) => switch (side) {
    ScreenSide.left => ScreenSide.right,
    ScreenSide.right => ScreenSide.left,
    ScreenSide.top => ScreenSide.bottom,
    ScreenSide.bottom => ScreenSide.top,
  };

  void _save() {
    final id =
        widget.original?.id ??
        widget.extension.plans.fold(0, (highest, plan) => plan.id > highest ? plan.id : highest) + 1;
    Navigator.of(context).pop(
      DisplayPlan(
        id: id,
        name: _name.text.trim(),
        peer: widget.peer.fingerprint,
        localSource: _localSource,
        sourceScreen: _source,
        viewerScreen: _viewer,
        side: _side,
        primary: _primary,
        alignment: _alignment,
        audio: _audio,
        shortcut: _shortcut == 0 ? null : _shortcut,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final strings = context.l10n;
    final used = {
      ...widget.extension.shortcuts.values,
      for (final plan in widget.extension.plans)
        if (plan.id != widget.original?.id && plan.shortcut != null) plan.shortcut!,
    };
    final valid =
        _name.text.trim().isNotEmpty &&
        (_source == null || _sourceScreens.any((s) => s.id == _source)) &&
        (_viewer == null || _viewerScreens.any((s) => s.id == _viewer));
    final sourceName = _localSource ? widget.localName : widget.peer.displayName;
    final viewerName = _localSource ? widget.peer.displayName : widget.localName;
    Widget screenCard(String name, bool main) => Expanded(
      child: Container(
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: Theme.of(context).colorScheme.surfaceContainerHighest,
          borderRadius: BorderRadius.circular(12),
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.desktop_windows_outlined),
            const SizedBox(height: 6),
            Text(name, maxLines: 2, overflow: TextOverflow.ellipsis),
            Text(main ? strings.displayPlanPreviewMain : strings.displayPlanPreviewSecondary),
          ],
        ),
      ),
    );
    final sourceCard = screenCard(sourceName, _primary == DisplayPrimary.source);
    final viewerCard = screenCard(viewerName, _primary == DisplayPrimary.viewer);
    final first = _side == ScreenSide.left || _side == ScreenSide.top ? viewerCard : sourceCard;
    final last = _side == ScreenSide.left || _side == ScreenSide.top ? sourceCard : viewerCard;
    final preview = _side == ScreenSide.left || _side == ScreenSide.right
        ? Row(children: [first, const SizedBox(width: 8), last])
        : SizedBox(
            height: 220,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [first, const SizedBox(height: 8), last],
            ),
          );
    return AlertDialog(
      backgroundColor: SfColors.of(context).card,
      title: Text(widget.original == null ? strings.displayPlanAdd : strings.displayPlanEdit),
      content: SizedBox(
        width: 540,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              TextField(
                key: const ValueKey('display-plan-name'),
                controller: _name,
                maxLength: 64,
                decoration: InputDecoration(labelText: strings.displayPlanName),
                onChanged: (_) => setState(() {}),
              ),
              const SizedBox(height: 12),
              DropdownButtonFormField<bool>(
                key: const ValueKey('display-plan-desktop'),
                initialValue: _localSource,
                isExpanded: true,
                decoration: InputDecoration(labelText: strings.displayPlanDesktop),
                items: [
                  DropdownMenuItem(value: true, child: Text(widget.localName)),
                  DropdownMenuItem(value: false, child: Text(widget.peer.displayName)),
                ],
                onChanged: (value) => setState(() {
                  if (value == null || value == _localSource) return;
                  _localSource = value;
                  final previous = _source;
                  _source = _viewer;
                  _viewer = previous;
                  _side = _opposite(_side);
                }),
              ),
              const SizedBox(height: 18),
              _screenPicker(
                strings.displayPlanSourceScreen,
                _sourceScreens,
                _source,
                (v) => _source = v,
                'display-plan-source-screen',
              ),
              const SizedBox(height: 18),
              _screenPicker(
                strings.displayPlanViewerScreen,
                _viewerScreens,
                _viewer,
                (v) => _viewer = v,
                'display-plan-viewer-screen',
              ),
              const SizedBox(height: 18),
              DropdownButtonFormField<DisplayPrimary>(
                key: const ValueKey('display-plan-primary'),
                initialValue: _primary,
                isExpanded: true,
                decoration: InputDecoration(labelText: strings.displayPlanPrimary),
                items: [
                  DropdownMenuItem(value: DisplayPrimary.source, child: Text(strings.displayPlanSourcePrimary)),
                  DropdownMenuItem(value: DisplayPrimary.viewer, child: Text(strings.displayPlanViewerPrimary)),
                ],
                onChanged: (value) => setState(() => _primary = value ?? _primary),
              ),
              const SizedBox(height: 18),
              DropdownButtonFormField<ScreenSide>(
                key: ValueKey('display-plan-position-$_side'),
                initialValue: _side,
                isExpanded: true,
                decoration: InputDecoration(labelText: strings.displayPlanPosition),
                items: [
                  for (final side in ScreenSide.values)
                    DropdownMenuItem(
                      value: side,
                      child: Text(switch (side) {
                        ScreenSide.left => strings.sideLeft,
                        ScreenSide.right => strings.sideRight,
                        ScreenSide.top => strings.sideTop,
                        ScreenSide.bottom => strings.sideBottom,
                      }),
                    ),
                ],
                onChanged: (side) => setState(() => _side = side ?? _side),
              ),
              const SizedBox(height: 18),
              preview,
              const SizedBox(height: 18),
              AlignmentRuler(
                value: _alignment,
                localName: sourceName,
                remoteName: viewerName,
                side: _side,
                onChanged: (value) => setState(() => _alignment = value),
                onPick: widget.extension.rulerPeers.contains(widget.peer.fingerprint)
                    ? () => AppScope.of(context).pickAlignment(
                        widget.peer.fingerprint,
                        localScreen: _localSource ? _source : _viewer,
                        peerScreen: _localSource ? _viewer : _source,
                        horizontal: _side == ScreenSide.top || _side == ScreenSide.bottom,
                        reversed: !_localSource,
                      )
                    : null,
                onCancel: AppScope.of(context).cancelAlignment,
              ),
              const SizedBox(height: 18),
              DropdownButtonFormField<int>(
                initialValue: _shortcut,
                isExpanded: true,
                decoration: InputDecoration(labelText: strings.displayShortcuts),
                items: [
                  DropdownMenuItem(value: 0, child: Text(strings.deviceOff)),
                  for (var key = 48; key <= 57; key++)
                    if (!used.contains(key))
                      DropdownMenuItem(value: key, child: Text('Ctrl+Alt+Shift+${String.fromCharCode(key)}')),
                ],
                onChanged: (key) => setState(() => _shortcut = key ?? 0),
              ),
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                title: Text(strings.displayAudio),
                value: _audio,
                onChanged: (value) => setState(() => _audio = value),
              ),
              Text(strings.displayAudioHint, style: Theme.of(context).textTheme.bodySmall),
              Text(strings.displayPlanHint, style: Theme.of(context).textTheme.bodySmall),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.of(context).pop(), child: Text(strings.commonCancel)),
        FilledButton(
          key: const ValueKey('display-plan-save'),
          onPressed: valid ? _save : null,
          child: Text(strings.commonSave),
        ),
      ],
    );
  }
}
