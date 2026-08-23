#!/usr/bin/env perl

use strict;
use warnings;
use Digest::SHA qw(sha256);

@ARGV == 2 or die "usage: $0 MNU_ELF OUTPUT\n";
my ($image_file, $output_file) = @ARGV;

open my $image_fh, '<:raw', $image_file or die "cannot read $image_file: $!\n";
local $/;
my $image = <$image_fh>;
close $image_fh or die "cannot close $image_file: $!\n";

my $path = '\\EFI\\MBOOT\\MNU.ELF';
my $header_size = 32;
my $entry_size = 160;
my $header = pack(
    'a8 v v v v V V Q<',
    "MBLHV1\0\0", 1, $header_size, $entry_size, 1,
    $header_size + $entry_size, 0, 0,
);
my $entry = pack(
    'V v v Q< v a14 Q< a32 v a6 a80',
    1, 1, 3, 2 * 1024 * 1024, 1, '', 0, sha256($image), length($path), '', $path,
);

open my $output_fh, '>:raw', $output_file or die "cannot write $output_file: $!\n";
print {$output_fh} $header, $entry or die "cannot write $output_file: $!\n";
close $output_fh or die "cannot close $output_file: $!\n";
