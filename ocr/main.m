// anveesa-ocr: reads an image file, runs Apple Vision text recognition,
// prints JSON lines to stdout. Usage: anveesa-ocr <image-path>
#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#import <Vision/Vision.h>

// NOTE: bounding boxes are already emitted below as normalized [x,y,w,h]
// with a bottom-left origin (Vision's convention) — consumed by ocr.rs.
int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc < 2) {
            fprintf(stderr, "usage: anveesa-ocr <image-path>\n");
            return 2;
        }
        NSString *path = [NSString stringWithUTF8String:argv[1]];
        NSImage *img = [[NSImage alloc] initWithContentsOfFile:path];
        if (!img) {
            fprintf(stderr, "could not load image: %s\n", argv[1]);
            return 1;
        }
        CGImageRef cg = [img CGImageForProposedRect:nil context:nil hints:nil];
        if (!cg) {
            fprintf(stderr, "could not decode image\n");
            return 1;
        }

        VNRecognizeTextRequest *req = [[VNRecognizeTextRequest alloc] init];
        req.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
        req.usesLanguageCorrection = YES;
        if (@available(macOS 13.0, *)) {
            req.automaticallyDetectsLanguage = YES;
        }
        req.recognitionLanguages = @[ @"zh-Hans", @"zh-Hant", @"ja-JP", @"ko-KR", @"en-US" ];

        VNImageRequestHandler *handler =
            [[VNImageRequestHandler alloc] initWithCGImage:cg options:@{}];
        NSError *err = nil;
        if (![handler performRequests:@[ req ] error:&err]) {
            fprintf(stderr, "vision error: %s\n", err.localizedDescription.UTF8String);
            return 1;
        }

        NSMutableArray *lines = [NSMutableArray array];
        for (VNRecognizedTextObservation *obs in req.results) {
            VNRecognizedText *cand = [[obs topCandidates:1] firstObject];
            if (!cand) continue;
            // boundingBox is normalized with a bottom-left origin (Vision convention)
            CGRect b = obs.boundingBox;
            [lines addObject:@{
                @"text" : cand.string,
                @"confidence" : @(cand.confidence),
                @"box" : @[ @(b.origin.x), @(b.origin.y), @(b.size.width), @(b.size.height) ],
            }];
        }

        NSData *data = [NSJSONSerialization dataWithJSONObject:@{@"lines" : lines}
                                                       options:0
                                                         error:&err];
        if (!data) {
            fprintf(stderr, "json error: %s\n", err.localizedDescription.UTF8String);
            return 1;
        }
        fwrite(data.bytes, 1, data.length, stdout);
        printf("\n");
    }
    return 0;
}
