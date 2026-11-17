import 'package:flutter/material.dart';

import '../ipc/messages.dart';
import '../l10n/l10n.dart';
import '../state/app_scope.dart';

Future<EdgeAlignment?> showAlignmentDialog(
  BuildContext context, {
  required String fingerprint,
  required String localName,
  required String remoteName,
  required ScreenSide side,
  required EdgeAlignment initial,
}) {
  var value = initial;
  final state = AppScope.of(context);
  return showDialog<EdgeAlignment>(
    context: context,
    builder: (context) => StatefulBuilder(
      builder: (context, update) => AlertDialog(
        title: Text(context.l10n.alignmentTitle),
        content: SizedBox(
          width: 520,
          child: AlignmentRuler(
            value: value,
            localName: localName,
            remoteName: remoteName,
            side: side,
            onChanged: (v) => update(() => value = v),
            onPick: state.daemon?.displayExtension?.rulerPeers.contains(fingerprint) == true
                ? () =>
                      state.pickAlignment(fingerprint, horizontal: side == ScreenSide.top || side == ScreenSide.bottom)
                : null,
            onCancel: state.cancelAlignment,
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: Text(MaterialLocalizations.of(context).cancelButtonLabel),
          ),
          FilledButton(onPressed: () => Navigator.pop(context, value), child: Text(context.l10n.alignmentApply)),
        ],
      ),
    ),
  );
}

/// 取点发生在两台电脑的实际屏幕上；这里仅启动、取消与显示取点结果。
class AlignmentRuler extends StatefulWidget {
  const AlignmentRuler({
    super.key,
    required this.value,
    required this.localName,
    required this.remoteName,
    required this.side,
    required this.onChanged,
    this.onPick,
    this.onCancel,
  });
  final EdgeAlignment value;
  final String localName, remoteName;
  final ScreenSide side;
  final ValueChanged<EdgeAlignment> onChanged;
  final Future<EdgeAlignment?> Function()? onPick;
  final VoidCallback? onCancel;
  @override
  State<AlignmentRuler> createState() => _AlignmentRulerState();
}

class _AlignmentRulerState extends State<AlignmentRuler> {
  bool _busy = false;
  @override
  void dispose() {
    if (_busy) widget.onCancel?.call();
    super.dispose();
  }

  Future<void> _pick() async {
    setState(() => _busy = true);
    try {
      final selected = await widget.onPick?.call();
      if (mounted && selected != null) widget.onChanged(selected);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final l = context.l10n;
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(l.alignmentPickHint),
        const SizedBox(height: 16),
        Row(
          children: [
            const Icon(Icons.desktop_windows_outlined),
            const SizedBox(width: 8),
            Expanded(child: Text(widget.localName)),
            const Icon(Icons.swap_horiz),
            const SizedBox(width: 8),
            Expanded(child: Text(widget.remoteName)),
          ],
        ),
        const SizedBox(height: 16),
        FilledButton.icon(
          key: const ValueKey('alignment-pick'),
          onPressed: _busy || widget.onPick == null ? null : _pick,
          icon: Icon(_busy ? Icons.touch_app_outlined : Icons.straighten),
          label: Text(_busy ? l.alignmentPicking : l.alignmentPickStart),
        ),
        if (_busy) ...[
          const SizedBox(height: 10),
          Text(l.alignmentPickSteps),
          TextButton(onPressed: widget.onCancel, child: Text(MaterialLocalizations.of(context).cancelButtonLabel)),
        ] else if (widget.onPick == null) ...[
          const SizedBox(height: 8),
          Text(l.alignmentPickUpgrade),
        ],
        const SizedBox(height: 10),
        Text(widget.value.isDefault ? l.alignmentDefault : l.alignmentPicked),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            onPressed: _busy ? null : () => widget.onChanged(const EdgeAlignment()),
            child: Text(l.alignmentReset),
          ),
        ),
      ],
    );
  }
}
