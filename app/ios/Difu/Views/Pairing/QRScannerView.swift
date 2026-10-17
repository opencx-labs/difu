import AVFoundation
import SwiftUI
import UIKit

/// Camera QR scanner with permission handling.
struct ScannerScreen: View {
    let onCode: (String) -> Void

    @Environment(\.openURL) private var openURL
    @State private var status = AVCaptureDevice.authorizationStatus(for: .video)
    @State private var unavailable = false

    var body: some View {
        Group {
            if unavailable {
                ContentUnavailableView(
                    "No camera available", systemImage: "camera.badge.ellipsis",
                    description: Text("Paste the pairing link instead."))
            } else {
                switch status {
                case .authorized:
                    QRScannerView(onCode: onCode, onUnavailable: { unavailable = true })
                        .ignoresSafeArea()
                        .overlay { viewfinder }
                case .notDetermined:
                    ProgressView()
                        .task {
                            let granted = await AVCaptureDevice.requestAccess(for: .video)
                            status = granted ? .authorized : .denied
                        }
                default:
                    ContentUnavailableView {
                        Label("Camera access is off", systemImage: "camera.fill")
                    } description: {
                        Text("Allow camera access in Settings to scan the pairing code, or paste the pairing link instead.")
                    } actions: {
                        Button("Open Settings") {
                            if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                        }
                        .buttonStyle(.borderedProminent)
                    }
                }
            }
        }
    }

    private var viewfinder: some View {
        VStack(spacing: 18) {
            Spacer()
            RoundedRectangle(cornerRadius: 24)
                .strokeBorder(.white.opacity(0.85), lineWidth: 3)
                .frame(width: 250, height: 250)
                .shadow(color: .black.opacity(0.4), radius: 8)
            Text("Point at the QR code in /remote-control")
                .font(.callout.weight(.medium))
                .foregroundStyle(.white)
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .background(.black.opacity(0.55), in: Capsule())
            Spacer()
        }
        .allowsHitTesting(false)
    }
}

/// `AVCaptureMetadataOutput` QR reader wrapped for SwiftUI.
struct QRScannerView: UIViewControllerRepresentable {
    let onCode: (String) -> Void
    let onUnavailable: () -> Void

    func makeUIViewController(context: Context) -> ScannerViewController {
        let controller = ScannerViewController()
        controller.onCode = onCode
        controller.onUnavailable = onUnavailable
        return controller
    }

    func updateUIViewController(_ controller: ScannerViewController, context: Context) {
        controller.onCode = onCode
        controller.onUnavailable = onUnavailable
    }
}

final class ScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onCode: ((String) -> Void)?
    var onUnavailable: (() -> Void)?

    private let session = AVCaptureSession()
    private let sessionQueue = DispatchQueue(label: "app.difu.ios.scanner")
    private var preview: AVCaptureVideoPreviewLayer?
    private var lastCode: String?
    private var lastScan = Date.distantPast

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        guard let device = AVCaptureDevice.default(for: .video),
            let input = try? AVCaptureDeviceInput(device: device),
            session.canAddInput(input)
        else {
            DispatchQueue.main.async { [weak self] in self?.onUnavailable?() }
            return
        }
        session.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            DispatchQueue.main.async { [weak self] in self?.onUnavailable?() }
            return
        }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]

        let preview = AVCaptureVideoPreviewLayer(session: session)
        preview.videoGravity = .resizeAspectFill
        view.layer.addSublayer(preview)
        self.preview = preview
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.bounds
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        let session = SendableSession(session: session)
        sessionQueue.async { if !session.session.isRunning { session.session.startRunning() } }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        let session = SendableSession(session: session)
        sessionQueue.async { if session.session.isRunning { session.session.stopRunning() } }
    }

    nonisolated func metadataOutput(
        _ output: AVCaptureMetadataOutput, didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        let value = metadataObjects
            .compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
            .first
        guard let value else { return }
        MainActor.assumeIsolated { self.found(value) }
    }

    private func found(_ value: String) {
        // Ignore repeats of the same code for a moment.
        if value == lastCode, Date().timeIntervalSince(lastScan) < 2.5 { return }
        lastCode = value
        lastScan = Date()
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        onCode?(value)
    }
}

/// Lets the capture session hop to its own queue.
private struct SendableSession: @unchecked Sendable {
    let session: AVCaptureSession
}
