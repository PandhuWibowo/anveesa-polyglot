// anveesa-audio: captures system audio via ScreenCaptureKit and streams
// raw f32le mono 16 kHz PCM to stdout. Logs go to stderr. Exits when the
// parent closes stdin (so orphaned helpers never linger).
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>

static SCStream *gStream = nil;
static id gOutput = nil;

@interface AudioOutput : NSObject <SCStreamOutput, SCStreamDelegate>
@end

@implementation AudioOutput
- (void)stream:(SCStream *)stream
    didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
                   ofType:(SCStreamOutputType)type {
    if (type != SCStreamOutputTypeAudio) return;
    CMBlockBufferRef blockBuffer = NULL;
    AudioBufferList abl;
    OSStatus st = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sampleBuffer, NULL, &abl, sizeof(abl), NULL, NULL,
        kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, &blockBuffer);
    if (st != noErr) return;
    // channelCount is 1, so a single buffer of interleaved f32 samples
    if (abl.mNumberBuffers >= 1 && abl.mBuffers[0].mData != NULL) {
        fwrite(abl.mBuffers[0].mData, 1, abl.mBuffers[0].mDataByteSize, stdout);
        fflush(stdout);
    }
    if (blockBuffer) CFRelease(blockBuffer);
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    fprintf(stderr, "stream stopped: %s\n", error.localizedDescription.UTF8String);
    exit(1);
}
@end

int main(void) {
    @autoreleasepool {
        // exit when the parent process closes our stdin
        dispatch_source_t stdinSrc = dispatch_source_create(
            DISPATCH_SOURCE_TYPE_READ, STDIN_FILENO, 0,
            dispatch_get_global_queue(QOS_CLASS_UTILITY, 0));
        dispatch_source_set_event_handler(stdinSrc, ^{
            char buf[256];
            if (read(STDIN_FILENO, buf, sizeof(buf)) <= 0) exit(0);
        });
        dispatch_resume(stdinSrc);

        [SCShareableContent
            getShareableContentWithCompletionHandler:^(SCShareableContent *content,
                                                       NSError *error) {
            if (error || content.displays.count == 0) {
                fprintf(stderr, "no shareable content: %s\n",
                        error ? error.localizedDescription.UTF8String : "no displays");
                fprintf(stderr,
                        "grant Screen Recording (System Audio) permission in "
                        "System Settings > Privacy & Security > Screen Recording\n");
                exit(1);
            }
            SCDisplay *display = content.displays.firstObject;
            SCContentFilter *filter =
                [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];

            SCStreamConfiguration *cfg = [[SCStreamConfiguration alloc] init];
            cfg.capturesAudio = YES;
            cfg.excludesCurrentProcessAudio = YES;
            cfg.sampleRate = 16000;
            cfg.channelCount = 1;
            // we never add a video output, keep the video side minimal anyway
            cfg.width = 2;
            cfg.height = 2;
            cfg.minimumFrameInterval = CMTimeMake(1, 1);

            gOutput = [[AudioOutput alloc] init];
            gStream = [[SCStream alloc] initWithFilter:filter
                                         configuration:cfg
                                              delegate:gOutput];
            NSError *err = nil;
            BOOL ok = [gStream addStreamOutput:gOutput
                                          type:SCStreamOutputTypeAudio
                            sampleHandlerQueue:dispatch_queue_create("audio", NULL)
                                         error:&err];
            if (!ok) {
                fprintf(stderr, "addStreamOutput failed: %s\n",
                        err.localizedDescription.UTF8String);
                exit(1);
            }
            [gStream startCaptureWithCompletionHandler:^(NSError *e) {
                if (e) {
                    fprintf(stderr, "start failed: %s\n",
                            e.localizedDescription.UTF8String);
                    exit(1);
                }
                fprintf(stderr, "capturing system audio: f32le mono 16000 Hz\n");
            }];
        }];
        dispatch_main();
    }
}
