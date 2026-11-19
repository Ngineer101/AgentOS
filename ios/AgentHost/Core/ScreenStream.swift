import AVFoundation
import SwiftUI
import UIKit

/// Decodes screen-relay's H.264 (Annex B access units, SPS and PPS on
/// keyframes, no B-frames) onto an `AVSampleBufferDisplayLayer`, which does
/// the hardware decoding. Call `enqueue` from one serial queue.
final class H264Renderer: @unchecked Sendable {
    let layer = AVSampleBufferDisplayLayer()
    private var format: CMVideoFormatDescription?
    private var parameterSets: (Data, Data)?
    private var sps: Data?
    private var pps: Data?

    init() {
        layer.videoGravity = .resize
        layer.backgroundColor = UIColor.black.cgColor
    }

    /// Show one access unit. False until the first keyframe, or if it
    /// could not be parsed; the frame is still ACKed either way.
    func enqueue(_ accessUnit: Data) -> Bool {
        var units: [Data] = []
        for nal in Self.nalUnits(accessUnit) {
            switch nal[nal.startIndex] & 0x1F {
            case 7: sps = nal
            case 8: pps = nal
            case 9: continue // access unit delimiter
            default: units.append(nal)
            }
        }
        if let sps, let pps, parameterSets.map({ $0 != (sps, pps) }) ?? true {
            format = Self.formatDescription(sps: sps, pps: pps)
            parameterSets = (sps, pps)
        }
        guard let format, !units.isEmpty else { return false }
        var avcc = Data(capacity: accessUnit.count + units.count * 4)
        for nal in units {
            var length = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &length) { avcc.append(contentsOf: $0) }
            avcc.append(nal)
        }
        guard let sample = Self.sampleBuffer(avcc, format: format) else { return false }
        let renderer = layer.sampleBufferRenderer
        if renderer.status == .failed { renderer.flush() }
        renderer.enqueue(sample)
        return true
    }

    func reset() {
        layer.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
        format = nil
        parameterSets = nil
    }

    /// NAL units without their start codes.
    static func nalUnits(_ data: Data) -> [Data] {
        let bytes = [UInt8](data)
        var starts: [Int] = []
        var i = 0
        while i + 2 < bytes.count {
            if bytes[i] == 0, bytes[i + 1] == 0, bytes[i + 2] == 1 {
                starts.append(i + 3)
                i += 3
            } else {
                i += 1
            }
        }
        var units: [Data] = []
        for (k, start) in starts.enumerated() {
            var end = k + 1 < starts.count ? starts[k + 1] - 3 : bytes.count
            // A 4-byte start code leaves a zero behind the previous unit.
            while k + 1 < starts.count, end > start, bytes[end - 1] == 0 { end -= 1 }
            if end > start { units.append(Data(bytes[start..<end])) }
        }
        return units
    }

    private static func formatDescription(sps: Data, pps: Data) -> CMVideoFormatDescription? {
        var format: CMVideoFormatDescription?
        let status = sps.withUnsafeBytes { s in
            pps.withUnsafeBytes { p in
                let pointers = [
                    s.bindMemory(to: UInt8.self).baseAddress!,
                    p.bindMemory(to: UInt8.self).baseAddress!,
                ]
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault,
                    parameterSetCount: 2,
                    parameterSetPointers: pointers,
                    parameterSetSizes: [sps.count, pps.count],
                    nalUnitHeaderLength: 4,
                    formatDescriptionOut: &format
                )
            }
        }
        return status == noErr ? format : nil
    }

    private static func sampleBuffer(_ data: Data, format: CMVideoFormatDescription) -> CMSampleBuffer? {
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: data.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0,
            dataLength: data.count, flags: 0, blockBufferOut: &block
        ) == kCMBlockBufferNoErr, let block else { return nil }
        let copied = data.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: data.count)
        }
        guard copied == kCMBlockBufferNoErr else { return nil }
        var sample: CMSampleBuffer?
        var size = data.count
        guard CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format,
            sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil,
            sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample
        ) == noErr, let sample else { return nil }
        // Live video: show each frame as it comes, no presentation clock.
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
            )
        }
        return sample
    }
}

/// Receives frames on the Rust layer's thread, draws them in order and ACKs
/// each one with how long it waited here, so screen-relay's rate control can
/// tell a busy phone from a slow network.
final class ScreenBridge: ScreenListener, @unchecked Sendable {
    private let renderer: H264Renderer
    private weak var stream: ScreenStream?
    private let queue = DispatchQueue(label: "dev.agenthost.screen.decode", qos: .userInteractive)
    private let lock = NSLock()
    private var session: ScreenSession?
    /// Frames drawn before `ScreenSession.open` returned: ACKed on `attach`.
    private var unacked: [(UInt32, UInt32, UInt32)] = []

    init(renderer: H264Renderer, stream: ScreenStream) {
        self.renderer = renderer
        self.stream = stream
    }

    func attach(_ s: ScreenSession) {
        lock.lock()
        session = s
        let pending = unacked
        unacked = []
        lock.unlock()
        for (g, q, w) in pending { s.ack(generation: g, sequence: q, waitUs: w) }
    }

    func onFrame(frame: ScreenFrame) {
        let arrived = DispatchTime.now().uptimeNanoseconds
        queue.async { [self] in
            let shown = renderer.enqueue(frame.data)
            let waited = UInt32(min(UInt64(UInt32.max), (DispatchTime.now().uptimeNanoseconds - arrived) / 1000))
            lock.lock()
            let s = session
            if s == nil { unacked.append((frame.generation, frame.sequence, waited)) }
            lock.unlock()
            s?.ack(generation: frame.generation, sequence: frame.sequence, waitUs: waited)
            let size = CGSize(width: Int(frame.width), height: Int(frame.height))
            DispatchQueue.main.async { [weak stream] in
                MainActor.assumeIsolated { stream?.didShow(size: size, decoded: shown) }
            }
        }
    }

    func onClosed(reason: String) {
        DispatchQueue.main.async { [weak stream] in
            MainActor.assumeIsolated { stream?.didEnd(reason) }
        }
    }
}

/// One look at the host's screen: connecting, live, or ended.
@MainActor
final class ScreenStream: ObservableObject {
    enum Phase: Equatable {
        case idle, connecting, live
        case ended(String)
    }

    @Published private(set) var phase: Phase = .idle
    /// The desktop in pixels; zero until the first frame.
    @Published private(set) var size: CGSize = .zero
    @Published private(set) var control = false
    @Published private(set) var mode = ScreenModeName.desktop
    @Published private(set) var framesPerSecond = 0

    let renderer = H264Renderer()
    private var session: ScreenSession?
    private var recent: [Date] = []
    #if DEBUG
    private var demoTimer: Timer?
    #endif

    var isLive: Bool { phase == .live }

    func start(model: AppModel, mode: String, control: Bool) {
        close()
        renderer.reset()
        size = .zero
        self.mode = mode
        self.control = control
        phase = .connecting
        #if DEBUG
        if model.isDemo {
            startDemo()
            return
        }
        #endif
        let bridge = ScreenBridge(renderer: renderer, stream: self)
        Task {
            do {
                let s = try await model.openScreen(mode: mode, control: control, listener: bridge)
                bridge.attach(s)
                if phase == .connecting || phase == .live {
                    session = s
                } else {
                    s.close()
                }
            } catch {
                phase = .ended(userMessage(for: error))
            }
        }
    }

    func didShow(size: CGSize, decoded: Bool) {
        guard phase == .connecting || phase == .live else { return }
        if decoded, phase == .connecting { phase = .live }
        if size != self.size { self.size = size }
        let now = Date()
        recent.append(now)
        recent.removeAll { now.timeIntervalSince($0) > 1 }
        framesPerSecond = recent.count
    }

    func didEnd(_ reason: String) {
        session = nil
        if phase != .idle { phase = .ended(reason) }
    }

    /// Close this viewer; the host stops screen-relay once nobody watches.
    func close() {
        session?.close()
        session = nil
        #if DEBUG
        demoTimer?.invalidate()
        demoTimer = nil
        #endif
        if phase == .connecting || phase == .live { phase = .idle }
    }

    // MARK: Input (positions are fractions of the desktop)

    func click(_ p: CGPoint) { session?.click(x: p.x, y: p.y, button: 1) }
    func rightClick(_ p: CGPoint) { session?.click(x: p.x, y: p.y, button: 3) }
    func scroll(_ p: CGPoint, notches: Int32) { session?.scroll(x: p.x, y: p.y, dx: 0, dy: notches) }
    func type(_ text: String) { session?.typeText(text: text) }
    func key(_ hid: UInt16, modifiers: [UInt16] = []) { session?.key(hid: hid, modifiers: modifiers) }

    // MARK: Demo (DEBUG only)

    #if DEBUG
    /// Plays `DemoScreen.h264` through the same renderer, at 30 fps.
    private func startDemo() {
        guard let url = Bundle.main.url(forResource: "DemoScreen", withExtension: "h264"),
              let clip = try? Data(contentsOf: url) else {
            phase = .ended("DemoScreen.h264 is missing from the app bundle")
            return
        }
        let units = Self.accessUnits(clip)
        let renderer = self.renderer
        let queue = DispatchQueue(label: "dev.agenthost.screen.demo")
        var next = 0
        demoTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30.0, repeats: true) { [weak self] _ in
            let unit = units[next % units.count]
            next += 1
            queue.async {
                let shown = renderer.enqueue(unit)
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { self?.didShow(size: CGSize(width: 1280, height: 800), decoded: shown) }
                }
            }
        }
    }

    /// Split a raw .h264 file (with access unit delimiters) into access units.
    private static func accessUnits(_ clip: Data) -> [Data] {
        var units: [Data] = []
        var current = Data()
        for nal in H264Renderer.nalUnits(clip) {
            if nal[nal.startIndex] & 0x1F == 9, !current.isEmpty {
                units.append(current)
                current = Data()
            }
            current.append(contentsOf: [0, 0, 0, 1])
            current.append(nal)
        }
        if !current.isEmpty { units.append(current) }
        return units
    }
    #endif
}
